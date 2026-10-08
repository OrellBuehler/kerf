<script lang="ts">
	import './layout.css';
	import favicon from '$lib/assets/favicon.svg';
	import { Toaster } from '$lib/components/ui/sonner';
	import { onMount } from 'svelte';
	import { installSliderFill } from '$lib/slider-fill';
	import { installErrorLogging } from '$lib/log';
	import { isEmbeddedPopout } from '$lib/popout-boot';

	let { children } = $props();

	// A page opened by another window is a detached panel's window that was answered
	// with the app by mistake: it stays blank instead of starting a second editor.
	const embedded = typeof window !== 'undefined' && isEmbeddedPopout(window);

	if (!embedded) installErrorLogging();
	onMount(() => {
		if (!embedded) installSliderFill();
	});
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>Kerf</title>
</svelte:head>

{#if !embedded}
	{@render children()}
	<Toaster />
{/if}
