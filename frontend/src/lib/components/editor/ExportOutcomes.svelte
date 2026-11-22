<script lang="ts">
	// The exports of this session, newest first: the files each wrote — where, how big, how long — with a way to
	// show it in its folder, or why it failed. The Deliver panel keeps this where the 4-second toast cannot, and
	// the export dialog shows the same rows (`ui.exportOutcomes`, `export-outcomes.ts`).
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import { revealPath } from '#lib/api';
	import { toast } from '#lib/notifications.svelte';
	import { failure } from '#lib/failure-copy';
	import { fileFacts, headline, when, type ExportOutcome, type WrittenFile } from '#lib/export-outcomes';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { tip } from '#lib/tip';

	let { outcomes, limit = 5 }: { outcomes: readonly ExportOutcome[]; limit?: number } = $props();

	// Ages are relative, so they have to keep moving while the list is up.
	let now = $state(Date.now());
	$effect(() => {
		const t = setInterval(() => (now = Date.now()), 20_000);
		return () => clearInterval(t);
	});

	const show = (path: string) =>
		void revealPath(path).catch((e) => toast.error(failure("Couldn't show the file in its folder", e)));
</script>

{#snippet fileRow(f: WrittenFile, named: boolean)}
	<div style="display:flex;align-items:center;gap:8px;min-width:0;padding-left:21px">
		<div style="flex:1;min-width:0;display:flex;flex-direction:column;gap:1px">
			{#if named}
				<span
					data-selectable
					use:tip={f.path}
					style="font-family:var(--font-mono);font-size:var(--text-sm);color:var(--text-primary);overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
					>{f.name}</span
				>
			{/if}
			<span
				data-selectable
				use:tip={f.path}
				style="font-size:var(--text-xs);color:var(--text-muted);overflow:hidden;text-overflow:ellipsis;white-space:nowrap"
			>
				{[fileFacts(f, (s) => timeDisplay.fmt(s)), f.folder].filter(Boolean).join(' · ')}
			</span>
		</div>
		<Btn variant="ghost" size="sm" icon="folder-open" title={f.path} onclick={() => show(f.path)}>Show in folder</Btn>
	</div>
{/snippet}

<ul style="list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:6px">
	{#each outcomes.slice(0, limit) as o (o.id)}
		<li
			data-outcome={o.ok ? 'ok' : 'failed'}
			style="padding:8px 10px;border-radius:var(--radius-sm);background:var(--surface-inset);border:var(--line-width) solid {o.ok
				? 'var(--border-subtle)'
				: 'color-mix(in srgb,var(--danger) 40%,transparent)'};display:flex;flex-direction:column;gap:6px"
		>
			<div style="display:flex;align-items:center;gap:7px;min-width:0">
				<Icon n={o.ok ? 'circle-check' : 'circle-x'} s={14} color={o.ok ? 'var(--success)' : 'var(--danger)'} />
				<!-- one file is named in the heading; several are counted, and each is named in its row -->
				<span
					data-selectable
					style="flex:1;min-width:0;font-size:var(--text-sm);font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;{o.ok
						? o.files.length === 1
							? 'font-family:var(--font-mono);color:var(--text-primary)'
							: 'color:var(--text-primary)'
						: 'color:var(--red-400)'}">{o.ok && o.files.length === 1 ? o.files[0].name : headline(o)}</span
				>
				<span style="flex:none;font-size:var(--text-xs);color:var(--text-muted)">{when(o.at, now)}</span>
			</div>
			{#if o.ok}
				{#each o.files as f (f.path)}
					{@render fileRow(f, o.files.length > 1)}
				{/each}
				{#if o.notes}
					<div
						data-selectable
						style="padding-left:21px;font-size:var(--text-xs);line-height:1.45;color:{o.over
							? 'var(--warning)'
							: 'var(--text-muted)'};white-space:pre-line"
					>
						{o.notes}
					</div>
				{/if}
			{:else}
				<div
					data-selectable
					role="alert"
					style="padding-left:21px;font-size:var(--text-sm);line-height:1.45;color:var(--text-secondary);overflow-wrap:anywhere"
				>
					{o.reason || 'No reason was given.'}
				</div>
			{/if}
		</li>
	{/each}
</ul>
