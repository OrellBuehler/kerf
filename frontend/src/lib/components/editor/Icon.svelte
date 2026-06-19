<script lang="ts">
	import { icons } from './icons';

	let {
		n,
		s = 16,
		color = undefined,
		style = ''
	}: { n: string; s?: number; color?: string; style?: string } = $props();

	const Cmp = $derived(icons[n]);
	const css = $derived(`display:inline-flex;flex:none;${color ? `color:${color};` : ''}${style}`);
</script>

{#if Cmp}
	<Cmp size={s} strokeWidth={2} style={css} />
{:else}
	<span style="width:{s}px;height:{s}px;{css}"></span>
{/if}
