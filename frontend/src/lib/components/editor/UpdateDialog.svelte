<script lang="ts">
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { updater } from '$lib/updater.svelte';
	import { editor } from '$lib/state.svelte';

	const u = updater;

	function fmtBytes(n: number): string {
		if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
		return `${(n / 1024 / 1024).toFixed(1)} MB`;
	}

	const downloaded = $derived(u.progress?.downloaded ?? 0);
	const total = $derived(u.progress?.total ?? null);
	const pct = $derived(u.fraction === null ? null : Math.round(u.fraction * 100));

	// Restarting throws away an unsaved project, so say so before offering it.
	const unsaved = $derived(!editor.saved);

	const title = $derived(
		u.phase === 'downloading'
			? 'Downloading update'
			: u.phase === 'ready'
				? 'Update installed'
				: u.phase === 'checking'
					? 'Checking for updates'
					: u.update
						? `Kerf ${u.update.version} is available`
						: 'Kerf is up to date'
	);
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
	role="dialog"
	aria-modal="true"
	tabindex="-1"
	onclick={() => u.phase !== 'downloading' && u.close()}
	onkeydown={(e) => {
		if (e.key === 'Escape' && u.phase !== 'downloading') u.close();
		e.stopPropagation();
	}}
	style="position:fixed;inset:0;z-index:60;background:rgba(0,0,0,.55);display:flex;align-items:center;justify-content:center;padding:24px"
>
	<div
		onclick={(e) => e.stopPropagation()}
		style="width:460px;max-width:100%;max-height:100%;display:flex;flex-direction:column;background:var(--surface-panel);border:1px solid var(--border-default);border-radius:var(--radius-md);box-shadow:var(--shadow-lg,0 24px 60px rgba(0,0,0,.5));overflow:hidden"
	>
		<div
			style="height:var(--toolbar-h);flex:none;display:flex;align-items:center;gap:8px;padding:0 14px;border-bottom:1px solid var(--border-default)"
		>
			<Icon n="download" s={15} color="var(--text-secondary)" />
			<span style="font:var(--type-ui);font-weight:600;color:var(--text-primary);flex:1">{title}</span>
			{#if u.phase !== 'downloading'}
				<Btn variant="ghost" size="sm" onclick={() => u.close()}>✕</Btn>
			{/if}
		</div>

		<div style="flex:1;overflow-y:auto;padding:14px 16px;display:flex;flex-direction:column;gap:10px">
			<div style="font-family:var(--font-mono);font-size:12px;color:var(--text-muted)">
				Installed {u.version || '…'}{#if u.update}
					&nbsp;→&nbsp;<span style="color:var(--kerf-300)">{u.update.version}</span>
				{/if}
				{#if u.update?.date}
					&nbsp;·&nbsp;{u.update.date.slice(0, 10)}
				{/if}
			</div>

			{#if u.phase === 'checking'}
				<div style="font-size:13px;color:var(--text-secondary)">Asking GitHub for the latest release…</div>
			{:else if u.phase === 'current'}
				<div style="font-size:13px;color:var(--text-secondary)">
					You're running the newest published release.
				</div>
			{:else if u.phase === 'ready'}
				<div style="font-size:13px;color:var(--text-secondary)">
					The new version is installed. Restart Kerf to use it.
				</div>
				{#if unsaved}
					<div
						style="font-size:12px;color:var(--warning);background:var(--warning-surface);border-radius:var(--radius-sm);padding:8px 10px"
					>
						⚠ This project has unsaved changes — save it before restarting.
					</div>
				{/if}
			{:else if u.phase === 'downloading'}
				<div style="display:flex;align-items:center;gap:10px">
					<div style="flex:1;height:6px;border-radius:3px;background:var(--surface-inset);overflow:hidden">
						<div
							style="height:100%;width:{pct ?? 30}%;background:var(--kerf-500);transition:width var(--dur-fast) linear"
						></div>
					</div>
					<span
						style="font-family:var(--font-mono);font-size:12px;color:var(--text-muted);min-width:110px;text-align:right"
					>
						{pct !== null ? `${pct}% · ` : ''}{fmtBytes(downloaded)}{total ? ` / ${fmtBytes(total)}` : ''}
					</span>
				</div>
			{:else if u.update?.notes}
				<div style="font-size:12px;color:var(--text-secondary);white-space:pre-wrap;max-height:220px;overflow-y:auto;background:var(--surface-inset);border-radius:var(--radius-sm);padding:10px 12px">{u.update.notes.trim()}</div>
			{/if}

			{#if u.error}
				<div
					style="font-size:12px;color:var(--red-400);background:color-mix(in srgb,var(--red-600) 14%,transparent);border-radius:var(--radius-sm);padding:8px 10px"
				>
					⚠ {u.error}
					{#if u.errorKind === 'install'}
						<div style="color:var(--text-muted);margin-top:4px">
							In-place updates need the AppImage build on Linux — install from the release page instead.
						</div>
					{/if}
				</div>
			{/if}
		</div>

		<div style="flex:none;border-top:1px solid var(--border-default);display:flex;align-items:center;gap:8px;padding:12px 16px">
			<Btn variant="ghost" size="sm" icon="external-link" onclick={() => u.openReleasePage()}>Release page</Btn>
			<div style="flex:1"></div>
			{#if u.phase === 'ready'}
				<Btn variant="ghost" size="md" onclick={() => u.close()}>Later</Btn>
				<Btn variant="primary" size="md" icon="refresh-cw" onclick={() => u.restart()}>Restart now</Btn>
			{:else if u.phase === 'downloading'}
				<span style="font-size:12px;color:var(--text-muted)">Kerf will keep running until you restart.</span>
			{:else if u.update}
				<Btn variant="ghost" size="md" onclick={() => u.close()}>Later</Btn>
				<Btn variant="primary" size="md" icon="download" onclick={() => u.install()}>
					{u.error ? 'Retry install' : 'Install update'}
				</Btn>
			{:else}
				<Btn
					variant="secondary"
					size="md"
					icon="refresh-cw"
					disabled={u.phase === 'checking'}
					onclick={() => u.check(false)}
				>
					{u.phase === 'checking' ? 'Checking…' : 'Check again'}
				</Btn>
			{/if}
		</div>
	</div>
</div>
