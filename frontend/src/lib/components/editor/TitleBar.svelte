<script lang="ts">
	import KerfMark from './KerfMark.svelte';
	import Badge from './Badge.svelte';
	import Icon from './Icon.svelte';
	import { editor } from '$lib/state.svelte';
	import { updater } from '$lib/updater.svelte';
	import { notifications } from '$lib/notifications.svelte';
	import { settings } from '$lib/settings.svelte';

	// An available update stays offered here after the dialog is dismissed;
	// otherwise the version label doubles as a manual "check for updates".
	const available = $derived(updater.update !== null);
</script>

<div
	style="height:var(--titlebar-h);display:flex;align-items:center;gap:10px;padding:0 12px;background:var(--surface-app);border-bottom:1px solid var(--border-default);flex:none;-webkit-app-region:drag"
>
	<div style="flex:1;text-align:center;display:flex;align-items:center;justify-content:center;gap:8px">
		<KerfMark size={15} />
		<span style="font:var(--type-label);color:var(--text-secondary)">{editor.projectName}</span>
		{#if editor.saved}
			<Badge tone="success" dot>Saved</Badge>
		{:else}
			<Badge tone="warning" dot>Unsaved</Badge>
		{/if}
	</div>
	<button
		onclick={() => settings.toggle()}
		title="Settings — how much of this machine Kerf may use (⌘,)"
		aria-label="Settings"
		style="-webkit-app-region:no-drag;display:inline-flex;align-items:center;justify-content:center;width:26px;height:22px;border-radius:var(--radius-sm);cursor:pointer;border:1px solid {settings.open
			? 'var(--border-strong)'
			: 'transparent'};background:{settings.open ? 'var(--surface-active)' : 'transparent'};color:{settings.open
			? 'var(--text-primary)'
			: 'var(--text-disabled)'}"
	>
		<Icon n="settings" s={13} color="currentColor" />
	</button>
	<!-- Toasts are gone in seconds; this is where they can be read afterwards.
	     The badge only turns red when something unread actually failed. -->
	<button
		data-notification-bell
		onclick={() => notifications.toggle()}
		title={notifications.unread
			? `${notifications.unread} unread notification${notifications.unread === 1 ? '' : 's'}`
			: 'Notifications'}
		aria-label="Notifications"
		style="-webkit-app-region:no-drag;position:relative;display:inline-flex;align-items:center;justify-content:center;width:26px;height:22px;border-radius:var(--radius-sm);cursor:pointer;border:1px solid {notifications.open
			? 'var(--border-strong)'
			: 'transparent'};background:{notifications.open ? 'var(--surface-active)' : 'transparent'};color:{notifications.open
			? 'var(--text-primary)'
			: 'var(--text-disabled)'}"
	>
		<Icon n="bell" s={13} color="currentColor" />
		{#if notifications.unread}
			<span
				style="position:absolute;top:0;right:0;min-width:13px;height:13px;padding:0 3px;border-radius:999px;display:grid;place-items:center;font-family:var(--font-mono);font-size:9px;line-height:1;color:var(--text-on-accent);background:{notifications.unreadProblem
					? 'var(--danger)'
					: 'var(--kerf-500)'}">{notifications.unread > 99 ? '99+' : notifications.unread}</span
			>
		{/if}
	</button>
	<button
		onclick={() => updater.open()}
		title={available
			? `Kerf ${updater.update?.version} is available — click to install`
			: `Kerf ${updater.version} — click to check for updates`}
		style="-webkit-app-region:no-drag;display:inline-flex;align-items:center;gap:5px;padding:2px 8px;border-radius:999px;cursor:pointer;font-family:var(--font-mono);font-size:11px;border:1px solid {available
			? 'var(--kerf-500)'
			: 'transparent'};background:{available
			? 'color-mix(in srgb,var(--kerf-500) 22%,transparent)'
			: 'transparent'};color:{available ? 'var(--text-primary)' : 'var(--text-disabled)'}"
	>
		{#if available}
			<Icon n="download" s={12} />
			{updater.update?.version}
		{:else if updater.phase === 'checking'}
			checking…
		{:else}
			{updater.version && updater.version !== 'dev' ? `v${updater.version}` : updater.version}
		{/if}
	</button>
	<span
		title={editor.currentPath ?? 'In-memory project — not yet saved'}
		style="font-family:var(--font-mono);font-size:11px;color:var(--text-disabled);max-width:280px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
		>{editor.currentPath ?? 'local · in-memory'}</span
	>
</div>
