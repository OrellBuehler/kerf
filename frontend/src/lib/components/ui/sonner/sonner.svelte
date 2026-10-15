<script lang="ts">
	import { Toaster as Sonner, type ToasterProps as SonnerProps } from "svelte-sonner";
	import { settings } from '#lib/settings.svelte';
	import { STATUS_LOOK } from '#lib/status-look';
	import Icon from '../../editor/Icon.svelte';

	let { ...restProps }: SonnerProps = $props();
</script>

<!-- Kerf's own theme decides light or dark (`ModeWatcher` is not mounted, so the OS did), the toasts sit
     above the status bar instead of on it, and the rest of the look is `layout.css`. The glyphs are the
     notification log's (`status-look.ts`), so a toast and the line it leaves there agree. -->
<Sonner
	theme={settings.theme.scheme}
	class="toaster group"
	offset={{ bottom: 'calc(var(--statusbar-h) + 12px)', right: 12 }}
	style="--normal-bg: var(--surface-raised); --normal-text: var(--text-primary); --normal-border: var(--border-strong); --border-radius: var(--radius-lg);"
	{...restProps}
>
	{#snippet loadingIcon()}
		<span class="kerf-spin"><Icon n="loader" s={16} /></span>
	{/snippet}
	{#snippet successIcon()}
		<Icon n={STATUS_LOOK.success.icon} s={16} />
	{/snippet}
	{#snippet errorIcon()}
		<Icon n={STATUS_LOOK.error.icon} s={16} />
	{/snippet}
	{#snippet infoIcon()}
		<Icon n={STATUS_LOOK.info.icon} s={16} />
	{/snippet}
	{#snippet warningIcon()}
		<Icon n={STATUS_LOOK.warning.icon} s={16} />
	{/snippet}
</Sonner>
