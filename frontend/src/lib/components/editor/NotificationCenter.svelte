<script lang="ts">
	import Icon from './Icon.svelte';
	import { notifications, type Notice, type NoticeKind } from '$lib/notifications.svelte';

	let el = $state<HTMLDivElement | null>(null);
	let filter = $state<'all' | 'unread' | 'errors'>('all');

	// Ages are shown relative, so they have to keep moving while the panel is up.
	let now = $state(Date.now());
	$effect(() => {
		if (!notifications.open) return;
		now = Date.now();
		const t = setInterval(() => (now = Date.now()), 15_000);
		return () => clearInterval(t);
	});

	const shown = $derived(
		notifications.items.filter((n) =>
			filter === 'unread' ? !n.read : filter === 'errors' ? n.kind === 'error' || n.kind === 'warning' : true
		)
	);

	const look: Record<NoticeKind, { icon: string; color: string }> = {
		success: { icon: 'check', color: 'var(--success)' },
		error: { icon: 'alert-triangle', color: 'var(--danger)' },
		warning: { icon: 'alert-triangle', color: 'var(--warning)' },
		info: { icon: 'lightbulb', color: 'var(--kerf-400)' },
		note: { icon: 'bell', color: 'var(--text-disabled)' }
	};

	function age(n: Notice): string {
		const s = Math.max(0, Math.round((now - n.at) / 1000));
		if (s < 60) return 'just now';
		if (s < 3600) return `${Math.floor(s / 60)}m ago`;
		if (s < 86_400) return `${Math.floor(s / 3600)}h ago`;
		return new Date(n.at).toLocaleDateString();
	}

	const clock = (n: Notice) => new Date(n.at).toLocaleTimeString();

	function onWindowPointerDown(e: PointerEvent) {
		if (!notifications.open) return;
		if (e.target instanceof Node) {
			if (el?.contains(e.target)) return;
			// The bell toggles the panel itself; closing here too would reopen it.
			if ((e.target as HTMLElement).closest?.('[data-notification-bell]')) return;
		}
		notifications.open = false;
	}
</script>

<svelte:window
	onpointerdown={onWindowPointerDown}
	onkeydown={(e) => {
		if (notifications.open && e.key === 'Escape') {
			e.preventDefault();
			notifications.open = false;
		}
	}}
/>

{#if notifications.open}
	<div
		bind:this={el}
		role="dialog"
		aria-label="Notifications"
		style="position:fixed;top:calc(var(--titlebar-h) + 4px);right:10px;z-index:70;width:380px;max-width:calc(100vw - 20px);max-height:min(60vh,520px);display:flex;flex-direction:column;background:var(--surface-panel);border:1px solid var(--border-strong);border-radius:var(--radius-md);box-shadow:var(--shadow-lg,0 24px 60px rgba(0,0,0,.5));overflow:hidden;font-family:var(--font-sans)"
	>
		<div
			style="flex:none;display:flex;align-items:center;gap:8px;padding:9px 10px 9px 12px;border-bottom:1px solid var(--border-default)"
		>
			<Icon n="bell" s={14} color="var(--text-secondary)" />
			<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary)">Notifications</span>
			{#if notifications.unread}
				<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)"
					>{notifications.unread} unread</span
				>
			{/if}
			<div style="flex:1"></div>
			<button
				onclick={() => notifications.markAllRead()}
				disabled={notifications.unread === 0}
				title="Mark every notification as read"
				style="height:22px;padding:0 8px;border-radius:var(--radius-full);border:1px solid var(--border-strong);background:var(--surface-raised);color:{notifications.unread
					? 'var(--text-secondary)'
					: 'var(--text-disabled)'};font-size:11px;cursor:{notifications.unread ? 'pointer' : 'default'}"
				>Mark all read</button
			>
			<button
				onclick={() => notifications.clear()}
				disabled={notifications.items.length === 0}
				title="Clear the log"
				style="height:22px;padding:0 8px;border-radius:var(--radius-full);border:1px solid var(--border-strong);background:var(--surface-raised);color:{notifications
					.items.length
					? 'var(--text-secondary)'
					: 'var(--text-disabled)'};font-size:11px;cursor:{notifications.items.length ? 'pointer' : 'default'}"
				>Clear</button
			>
		</div>

		<div style="flex:none;display:flex;gap:5px;padding:8px 12px;border-bottom:1px solid var(--border-subtle)">
			{#each [['all', 'All'], ['unread', 'Unread'], ['errors', 'Problems']] as const as [k, label] (k)}
				<button
					onclick={() => (filter = k)}
					style="height:22px;padding:0 9px;border-radius:var(--radius-full);font-size:11px;cursor:pointer;border:1px solid {filter ===
					k
						? 'var(--border-strong)'
						: 'transparent'};background:{filter === k ? 'var(--surface-active)' : 'transparent'};color:{filter === k
						? 'var(--text-primary)'
						: 'var(--text-secondary)'}">{label}</button
				>
			{/each}
		</div>

		<div style="flex:1;overflow-y:auto;min-height:0">
			{#if shown.length === 0}
				<div
					style="display:flex;flex-direction:column;align-items:center;gap:8px;padding:34px 16px;color:var(--text-disabled);text-align:center"
				>
					<Icon n="bell" s={20} />
					<span style="font-size:11.5px"
						>{notifications.items.length === 0
							? 'Nothing has been reported yet.'
							: filter === 'unread'
								? 'Everything here has been read.'
								: 'No errors or warnings.'}</span
					>
				</div>
			{:else}
				{#each shown as n (n.id)}
					<div
						style="display:flex;gap:9px;align-items:flex-start;padding:9px 10px 9px 9px;border-bottom:1px solid var(--border-subtle);border-left:2px solid {n.read
							? 'transparent'
							: look[n.kind].color};background:{n.read ? 'transparent' : 'var(--surface-hover)'}"
					>
						<span style="margin-top:1px;flex:none"><Icon n={look[n.kind].icon} s={13} color={look[n.kind].color} /></span>
						<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:3px">
							<span
								data-selectable
								style="font-size:12px;line-height:1.45;color:{n.read
									? 'var(--text-secondary)'
									: 'var(--text-primary)'};overflow-wrap:anywhere;user-select:text">{n.text}</span
							>
							<span
								title={clock(n)}
								style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">{age(n)}</span
							>
						</div>
						<button
							onclick={() => notifications.markRead(n.id, !n.read)}
							title={n.read ? 'Mark as unread' : 'Mark as read'}
							aria-label={n.read ? 'Mark as unread' : 'Mark as read'}
							style="flex:none;width:20px;height:20px;display:grid;place-items:center;border:none;background:none;cursor:pointer;color:{n.read
								? 'var(--text-disabled)'
								: look[n.kind].color}"
						>
							{#if n.read}
								<span style="width:7px;height:7px;border-radius:50%;border:1px solid currentColor"></span>
							{:else}
								<span style="width:7px;height:7px;border-radius:50%;background:currentColor"></span>
							{/if}
						</button>
						<button
							onclick={() => notifications.remove(n.id)}
							title="Remove"
							aria-label="Remove"
							style="flex:none;width:20px;height:20px;display:grid;place-items:center;border:none;background:none;cursor:pointer;color:var(--text-disabled)"
						>
							<Icon n="x" s={12} color="currentColor" />
						</button>
					</div>
				{/each}
			{/if}
		</div>
	</div>
{/if}
