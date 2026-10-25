<script lang="ts">
	// One tag as a chip: `key: value` with the key muted, or the plain value. In the bin it is `compact` (a row has
	// little room); in the dialog and the Inspector it is full size and takes an × to remove it. `tone` says what is
	// to become of it: `pending` is going on, `removed` is going off (struck through, the × brings it back).
	import Icon from './Icon.svelte';
	import { formatTag } from '#lib/tags';
	import { tip } from '#lib/tip';
	import type { Tag } from '#lib/types';

	let {
		tag,
		tone = 'neutral',
		compact = false,
		note = '',
		removeLabel = undefined,
		onremove = undefined
	}: {
		tag: Tag;
		tone?: 'neutral' | 'pending' | 'removed';
		compact?: boolean;
		/** A muted word after the tag: `all`, `some (2)`. */
		note?: string;
		/** The × button's name; given with `onremove`. */
		removeLabel?: string;
		onremove?: () => void;
	} = $props();

	const edge = $derived(
		tone === 'pending'
			? 'var(--kerf-500)'
			: tone === 'removed'
				? 'var(--border-default)'
				: compact
					? 'var(--border-subtle)'
					: 'var(--border-strong)'
	);
	const fill = $derived(
		tone === 'pending' ? 'color-mix(in srgb,var(--kerf-500) 18%,transparent)' : compact ? 'var(--surface-inset)' : 'var(--surface-hover)'
	);
</script>

<span
	data-tag-chip
	data-tone={tone}
	use:tip={formatTag(tag)}
	style="display:inline-flex;align-items:center;gap:4px;min-width:0;max-width:100%;box-sizing:border-box;height:{compact ? 17 : 22}px;padding:0 {onremove ? 3 : compact ? 6 : 8}px 0 {compact ? 6 : 8}px;border-radius:var(--radius-sm);border:var(--line-width) {tone === 'removed' ? 'dashed' : 'solid'} {edge};background:{fill};font-size:{compact ? 10 : 12}px;line-height:1;color:{tone === 'removed' ? 'var(--text-muted)' : 'var(--text-secondary)'}"
>
	<span
		style="min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;{tone === 'removed' ? 'text-decoration:line-through' : ''}"
		>{#if tag.key != null}<span style="color:var(--text-muted)">{tag.key}:</span>&nbsp;{/if}<span
			style="color:{tone === 'removed' ? 'inherit' : 'var(--text-primary)'}">{tag.value}</span
		></span
	>
	{#if note}<span style="flex:none;font-size:10px;color:var(--text-muted)">{note}</span>{/if}
	{#if onremove}
		<button
			type="button"
			class="k-ibtn"
			aria-label={removeLabel ?? `Remove ${formatTag(tag)}`}
			use:tip={tone === 'removed' ? 'Keep this tag' : (removeLabel ?? `Remove ${formatTag(tag)}`)}
			onclick={onremove}
			style="flex:none;width:16px;height:16px;border-radius:var(--radius-sm)"
		>
			<Icon n={tone === 'removed' ? 'rotate-ccw' : 'x'} s={10} />
		</button>
	{/if}
</span>
