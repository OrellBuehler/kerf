<script lang="ts">
	import Modal from './Modal.svelte';
	import Btn from './Btn.svelte';
	import { updater } from '$lib/updater.svelte';
	import { editor } from '$lib/state.svelte';
	import { confirmAction } from '$lib/api';
	import { errorView, installNote, updateTitle } from '$lib/update-copy';
	import { isLinux } from '$lib/keymap';

	const u = updater;

	function fmtBytes(n: number): string {
		if (n < 1024 * 1024) return `${Math.round(n / 1024)} KB`;
		return `${(n / 1024 / 1024).toFixed(1)} MB`;
	}

	const downloaded = $derived(u.progress?.downloaded ?? 0);
	const total = $derived(u.progress?.total ?? null);
	const pct = $derived(u.fraction === null ? null : Math.round(u.fraction * 100));

	// Restarting throws away an unsaved project, so say so before offering it.
	const unsaved = $derived(editor.hasUnsavedWork);

	async function restart() {
		if (unsaved && !(await confirmAction('This project has never been saved and will be lost on restart. Restart anyway?', 'Restart Kerf')))
			return;
		await u.restart();
	}

	const title = $derived(updateTitle(u.phase, u.update));
	const note = $derived(installNote(u.errorKind, isLinux()));
	const failure = $derived(u.error ? errorView(u.errorKind, u.error) : null);
</script>

<Modal title={title} icon="download" width={460} busy={u.phase === 'downloading'} rescue onClose={() => u.close()}>
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
					style="font-size:12px;color:var(--orange-400);background:var(--warning-surface);border-radius:var(--radius-sm);padding:8px 10px"
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

		{#if failure}
			<div
				style="font-size:12px;color:var(--red-400);background:color-mix(in srgb,var(--red-600) 14%,transparent);border-radius:var(--radius-sm);padding:8px 10px"
			>
				<span style="overflow-wrap:anywhere">⚠ {failure.headline}</span>
				{#if failure.details}
					<details style="margin-top:4px">
						<summary style="cursor:pointer;color:var(--text-muted)">Details</summary>
						<div data-selectable style="margin-top:4px;font-family:var(--font-mono);color:var(--text-secondary);overflow-wrap:anywhere">
							{failure.details}
						</div>
					</details>
				{/if}
				{#if note}
					<div style="color:var(--text-muted);margin-top:4px">{note}</div>
				{/if}
			</div>
		{/if}
	</div>
	{#snippet footer()}
		<div style="display:flex;align-items:center;gap:8px;padding:12px 16px">
			<Btn variant="ghost" size="sm" icon="external-link" onclick={() => u.openReleasePage()}>Release page</Btn>
			<div style="flex:1"></div>
			{#if u.phase === 'ready'}
				<Btn variant="ghost" size="md" onclick={() => u.close()}>Later</Btn>
				<Btn variant="primary" size="md" icon="refresh-cw" data-rescue onclick={restart}>Restart now</Btn>
			{:else if u.phase === 'downloading'}
				<span style="font-size:12px;color:var(--text-muted)">Kerf will keep running until you restart.</span>
			{:else if u.update}
				<Btn variant="ghost" size="md" onclick={() => u.close()}>Later</Btn>
				<Btn variant="primary" size="md" icon="download" data-rescue onclick={() => u.install()}>
					{u.error ? 'Retry install' : 'Install update'}
				</Btn>
			{:else}
				<Btn
					variant="secondary"
					size="md"
					icon="refresh-cw"
					data-rescue
					disabled={u.phase === 'checking'}
					onclick={() => u.check(false)}
				>
					{u.phase === 'checking' ? 'Checking…' : 'Check again'}
				</Btn>
			{/if}
		</div>
	{/snippet}
</Modal>
