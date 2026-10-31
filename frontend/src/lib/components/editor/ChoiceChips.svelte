<script lang="ts" generics="T extends string | number">
	// A radio group drawn as chips: one tab stop, the arrows move through it (`roving`), the chosen chip is
	// `aria-checked`. `follow: false` is for a choice that is expensive to make (Proxy size rebuilds every
	// proxy): the arrows only move focus and Space / Enter choose.
	import { roving } from '#lib/roving';
	import { tabStop } from '#lib/menu-nav';
	import { tip } from '#lib/tip';

	let {
		label,
		options,
		value,
		onpick,
		follow = true,
		testid = undefined
	}: {
		label: string;
		options: readonly { value: T; label: string; hint?: string; disabled?: boolean }[];
		value: T;
		onpick: (value: T) => void;
		follow?: boolean;
		/** `data-testid` is `<testid>-<value>`. */
		testid?: string;
	} = $props();

	const stop = $derived(tabStop(options.map((o) => ({ checked: o.value === value, enabled: !o.disabled }))));
</script>

<div role="radiogroup" aria-label={label} use:roving={{ follow }} style="display:flex;flex-wrap:wrap;gap:6px">
	{#each options as o, i (o.value)}
		<button
			type="button"
			role="radio"
			aria-checked={o.value === value}
			tabindex={i === stop ? 0 : -1}
			data-testid={testid ? `${testid}-${o.value}` : undefined}
			use:tip={o.hint}
			disabled={o.disabled}
			onclick={() => onpick(o.value)}
			class="k-chip">{o.label}</button
		>
	{/each}
</div>
