<script lang="ts">
	import KerfMark from './KerfMark.svelte';
	import Badge from './Badge.svelte';
	import Icon from './Icon.svelte';
	import WorkspaceTabs from './WorkspaceTabs.svelte';
	import MenuBar from './MenuBar.svelte';
	import Btn from './Btn.svelte';
	import { editor } from '$lib/state.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { workspace } from '$lib/workspace.svelte';
	import { proposalIndicator, reviewChipText } from '$lib/agent-status';
	import { clipsIn, exportButtonState } from '$lib/export-button';
	import { updater } from '$lib/updater.svelte';
	import { notifications } from '$lib/notifications.svelte';
	import { settings } from '$lib/settings.svelte';
	import type { ActionId } from '$lib/keymap';
	import type { MenuCommand } from '$lib/menus';
	import { tip } from '$lib/tip';

	let { onAction, onCommand }: { onAction: (id: ActionId) => void; onCommand: (command: MenuCommand) => void } = $props();

	// The six titles need about this much of the left cell beside the logo; narrower,
	// they become one "Menu" button (see `MenuBar`). It is the width of that cell, which
	// the workspace tabs and the right cluster take their share around — not the window.
	const MENU_FULL_PX = 320;
	let leftWidth = $state(0);
	const compact = $derived(leftWidth > 0 && leftWidth < MENU_FULL_PX);

	// The right cell holds the project's name and badge, a review chip, Export, the gear, the bell and the
	// version; narrower than this the chip is its icon and count (the name has the least to give up).
	let rightWidth = $state(0);

	// An available update stays offered here after the dialog is dismissed;
	// otherwise the version label doubles as a manual "check for updates".
	const available = $derived(updater.update !== null);

	/** A proposal the agent staged: in the Audio and Deliver workspaces the agent is only a tab, and this is
	 *  the one place that says something is waiting. */
	const proposal = $derived(proposalIndicator(editor.staged));
	const exporting = $derived(editor.exportRun);
	const exportPercent = $derived(Math.round((exporting?.progress?.fraction ?? 0) * 100));
	const exportState = $derived(
		exportButtonState({ clips: clipsIn(editor.liveTimeline), running: exporting !== null })
	);
</script>

<!-- Three cells so the workspace tabs sit on the centre line whatever else the
     bar holds: the logo and the menus on the left, the workspaces in the middle,
     the project's name and the app's own controls on the right. No rule under it:
     the dock starts where the bar ends. -->
<div
	style="height:var(--titlebar-h);display:grid;grid-template-columns:minmax(0,1fr) auto minmax(0,1fr);align-items:center;gap:10px;padding:0 12px;background:var(--surface-app);flex:none;-webkit-app-region:drag"
>
	<div bind:clientWidth={leftWidth} style="min-width:0;display:flex;align-items:center;gap:8px">
		<KerfMark size={18} />
		<MenuBar {compact} {onAction} {onCommand} />
	</div>
	<WorkspaceTabs />
	<div bind:clientWidth={rightWidth} style="min-width:0;display:flex;align-items:center;justify-content:flex-end;gap:8px">
		<!-- What the project is: its name and whether it has a file. It gives way first
		     when the window is narrow. -->
		<div style="min-width:0;flex:0 1 auto;display:flex;align-items:center;gap:8px;overflow:hidden">
			<span
				use:tip={editor.currentPath ?? 'In-memory project — not yet saved'}
				style="min-width:0;font:var(--type-ui);color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				>{editor.projectName}</span
			>
			{#if editor.saved}
				<Badge tone="success" dot>Saved</Badge>
			{:else if editor.hasUnsavedWork}
				<Badge tone="warning" dot>Unsaved</Badge>
			{/if}
		</div>
		{#if proposal}
			<button
				class="k-ibtn"
				data-agent
				aria-label={proposal.label}
				onclick={() => workspace.show('agent')}
				use:tip={proposal.tip}
				style="-webkit-app-region:no-drag;flex:none;gap:6px;height:28px;padding:0 9px;font:var(--type-ui);white-space:nowrap"
			>
				<Icon n="git-pull-request-arrow" s={13} />
				{reviewChipText(proposal, rightWidth)}
			</button>
		{/if}
		<!-- The app's goal had no button outside the Deliver workspace; it is File › Export… and Ctrl+E too. A
		     render that is under way shows here as well as in the status bar, and the button reopens its dialog. -->
		<!-- An empty cut has nothing to render, so the button is off and says so. It is `aria-disabled`, not `disabled`:
		     it stays focusable, so its reason — the tooltip — is there for the keyboard as well as the pointer. -->
		<Btn
			variant="primary"
			size="sm"
			icon={exporting ? undefined : 'upload'}
			aria-disabled={exportState.disabled ? 'true' : undefined}
			title={exportState.disabled
				? exportState.reason
				: exporting
					? { text: `Exporting ${exportPercent}%`, detail: 'Open the export dialog to follow or stop it.' }
					: settings.tip('Export…', 'file.export', 'Render the cut to a file')}
			onclick={() => {
				if (!exportState.disabled) ui.openExport();
			}}
			style="-webkit-app-region:no-drag"
		>
			{#if exporting}
				<span
					class="kerf-spin"
					style="width:10px;height:10px;border:1.5px solid currentColor;border-top-color:transparent;border-radius:50%"
				></span>
				{exporting.progress?.waiting ? 'Waiting…' : `${exportPercent}%`}
			{:else}
				Export
			{/if}
		</Btn>
		<button
			class="k-ibtn"
			data-on={settings.open ? '' : undefined}
			data-on-ink="primary"
			onclick={() => settings.toggle()}
			use:tip={settings.tip('Settings', 'app.settings', 'Preferences, appearance, the agent connection and keyboard shortcuts')}
			aria-label="Settings"
			style="-webkit-app-region:no-drag;width:28px;height:28px"
		>
			<Icon n="settings" s={13} color="currentColor" />
		</button>
		<!-- Toasts are gone in seconds; this is where they can be read afterwards.
		     The badge only turns red when something unread actually failed. -->
		<button
			class="k-ibtn"
			data-notification-bell
			data-on={notifications.open ? '' : undefined}
			data-on-ink="primary"
			onclick={() => notifications.toggle()}
			use:tip={notifications.unread
				? `${notifications.unread} unread notification${notifications.unread === 1 ? '' : 's'}`
				: 'Notifications'}
			aria-label={notifications.unread ? `Notifications, ${notifications.unread} unread` : 'Notifications'}
			style="-webkit-app-region:no-drag;position:relative;width:28px;height:28px"
		>
			<Icon n="bell" s={13} color="currentColor" />
			{#if notifications.unread}
				<span
					style="position:absolute;top:-3px;right:-4px;min-width:14px;height:14px;padding:0 3px;border-radius:999px;display:inline-flex;align-items:center;justify-content:center;gap:1px;box-shadow:0 0 0 1.5px var(--surface-app);font-family:var(--font-mono);font-size:var(--text-2xs);line-height:1;color:var(--text-on-accent);background:{notifications.unreadProblem
						? 'var(--danger)'
						: 'var(--kerf-500)'}"
					>{#if notifications.unreadProblem}<Icon n="alert-triangle" s={9} color="currentColor" />{/if}{notifications.unread > 99 ? '99+' : notifications.unread}</span
				>
			{/if}
		</button>
		<button
			class="k-ibtn"
			data-accent={available ? '' : undefined}
			onclick={() => updater.open()}
			use:tip={available
				? `Kerf ${updater.update?.version} is available — click to install`
				: `Kerf ${updater.version} — click to check for updates`}
			style="-webkit-app-region:no-drag;gap:5px;height:28px;padding:0 10px;border-color:var(--border-default);font-family:var(--font-mono);font-size:var(--text-xs)"
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
