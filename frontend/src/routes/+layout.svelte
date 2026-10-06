<script lang="ts">
	import './layout.css';
	import favicon from '$lib/assets/favicon.svg';
	import { Toaster } from '$lib/components/ui/sonner';
	import { onMount } from 'svelte';
	import { installSliderFill } from '$lib/slider-fill';
	import { installErrorLogging } from '$lib/log';

	let { children } = $props();

	installErrorLogging();
	onMount(() => installSliderFill());

	let { children } = $props();
	onMount(() => installSliderFill());
	import { installErrorLogging } from '$lib/log';

	let { children } = $props();

	installErrorLogging();
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>Kerf</title>
</svelte:head>

{@render children()}
<Toaster />
