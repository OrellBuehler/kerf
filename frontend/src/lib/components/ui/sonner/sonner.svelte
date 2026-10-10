<script lang="ts">
	import { Toaster as Sonner, type ToasterProps as SonnerProps } from "svelte-sonner";
	import { settings } from '$lib/settings.svelte';
	import Loader2Icon from '@lucide/svelte/icons/loader-2';
	import CircleCheckIcon from '@lucide/svelte/icons/circle-check';
	import OctagonXIcon from '@lucide/svelte/icons/octagon-x';
	import InfoIcon from '@lucide/svelte/icons/info';
	import TriangleAlertIcon from '@lucide/svelte/icons/triangle-alert';

	let { ...restProps }: SonnerProps = $props();
</script>

<!-- Kerf's own theme decides light or dark (`ModeWatcher` is not mounted, so the OS did), the toasts sit
     above the status bar instead of on it, and the rest of the look is `layout.css`. -->
<Sonner
	theme={settings.theme.scheme}
	class="toaster group"
	offset={{ bottom: 'calc(var(--statusbar-h) + 12px)', right: 12 }}
	style="--normal-bg: var(--surface-raised); --normal-text: var(--text-primary); --normal-border: var(--border-strong); --border-radius: var(--radius-lg);"
	{...restProps}
>
	{#snippet loadingIcon()}
		<Loader2Icon class="size-4 animate-spin" />
	{/snippet}
	{#snippet successIcon()}
		<CircleCheckIcon class="size-4" />
	{/snippet}
	{#snippet errorIcon()}
		<OctagonXIcon class="size-4" />
	{/snippet}
	{#snippet infoIcon()}
		<InfoIcon class="size-4" />
	{/snippet}
	{#snippet warningIcon()}
		<TriangleAlertIcon class="size-4" />
	{/snippet}
</Sonner>
