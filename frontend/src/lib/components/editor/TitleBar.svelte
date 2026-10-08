<script lang="ts">
	import KerfMark from './KerfMark.svelte';
	import Badge from './Badge.svelte';
	import Icon from './Icon.svelte';
	import WorkspaceTabs from './WorkspaceTabs.svelte';
	import MenuBar from './MenuBar.svelte';
	import { editor } from '$lib/state.svelte';
	import { updater } from '$lib/updater.svelte';
	import { notifications } from '$lib/notifications.svelte';
	import { settings } from '$lib/settings.svelte';
	import type { ActionId } from '$lib/keymap';
	import type { MenuCommand } from '$lib/menus';

	let { onAction, onCommand }: { onAction: (id: ActionId) => void; onCommand: (command: MenuCommand) => void } = $props();

	// The six titles need about this much of the left cell beside the logo; narrower,
	// they become one "Menu" button (see `MenuBar`). It is the width of that cell, which
	// the workspace tabs and the right cluster take their share around — not the window.
	const MENU_FULL_PX = 320;
	let leftWidth = $state(0);
	const compact = $derived(leftWidth > 0 && leftWidth < MENU_FULL_PX);

	// An available update stays offered here after the dialog is dismissed;
	// otherwise the version label doubles as a manual "check for updates".
	const available = $derived(updater.update !== null);
</script>

<!-- Three cells so the workspace tabs sit on the centre line whatever else the
     bar holds: the logo and the menus on the left, the workspaces in the middle,
     the project's name and the app's own controls on the right. No rule under it:
     the dock starts where the bar ends. -->
<div
	style="height:var(--titlebar-h);display:grid;grid-template-columns:minmax(0,1fr) auto minmax(0,1fr);align-items:center;gap:10px;padding:0 12px;background:var(--surface-app);flex:none;-webkit-app-region:drag"
>
	<div bind:clientWidth={leftWidth} style="min-width:0;display:flex;align-items:center;gap:8px">
		<KerfMark size={15} />
		<MenuBar {compact} {onAction} {onCommand} />
	</div>
	<WorkspaceTabs />
	<div style="min-width:0;display:flex;align-items:center;justify-content:flex-end;gap:10px">
		<!-- What the project is: its name and whether it has a file. It gives way first
		     when the window is narrow. -->
		<div style="min-width:0;flex:0 1 auto;display:flex;align-items:center;gap:8px;overflow:hidden">
			<span
				title={editor.currentPath ?? 'In-memory project — not yet saved'}
				style="min-width:0;font:var(--type-label);color:var(--text-secondary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				>{editor.projectName}</span
			>
			{#if editor.saved}
				<Badge tone="success" dot>Saved</Badge>
			{:else}
				<Badge tone="warning" dot>Unsaved</Badge>
			{/if}
		</div>
		<button
			onclick={() => settings.toggle()}
			title={settings.withShortcut('Settings — performance, appearance and keyboard shortcuts', 'app.settings')}
			aria-label="Settings"
			style="-webkit-app-region:no-drag;display:inline-flex;align-items:center;justify-content:center;width:26px;height:22px;border-radius:var(--radius-sm);cursor:pointer;border:var(--line-width) solid {settings.open
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
			style="-webkit-app-region:no-drag;position:relative;display:inline-flex;align-items:center;justify-content:center;width:26px;height:22px;border-radius:var(--radius-sm);cursor:pointer;border:var(--line-width) solid {notifications.open
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
			style="-webkit-app-region:no-drag;display:inline-flex;align-items:center;gap:5px;padding:2px 8px;border-radius:999px;cursor:pointer;font-family:var(--font-mono);font-size:11px;border:var(--line-width) solid {available
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
	</div>
</div>
