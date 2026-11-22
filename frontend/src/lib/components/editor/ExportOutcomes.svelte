<script lang="ts">
	// The exports of this session, newest first: the files each wrote — where, how big, how long — with a way to
	// show it in its folder, or why it failed. The Deliver panel keeps this where the 4-second toast cannot, and
	// the export dialog shows the same rows (`ui.exportOutcomes`, `export-outcomes.ts`).
	import Icon from './Icon.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import { revealPath } from '#lib/api';
	import { toast } from '#lib/notifications.svelte';
	import { failure } from '#lib/failure-copy';
	import { fileFacts, headline, reasonLines, when, type ExportOutcome, type WrittenFile } from '#lib/export-outcomes';
	import { copyText } from '#lib/clipboard';
	import { timeDisplay } from '#lib/time-display.svelte';
	import { tip } from '#lib/tip';

	/** `dense`: a file of a multi-file export is one line and its folder button an icon, so the newest of a four-shape export is not
	 *  a hundred and sixty pixels of a panel. A failure's reason is capped at `REASON_MAX` px and scrolls inside its own card. */
	let { outcomes, limit = 5, dense = false }: { outcomes: readonly ExportOutcome[]; limit?: number; dense?: boolean } = $props();

	const REASON_MAX = 72;

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
	{@const facts = [fileFacts(f, (s) => timeDisplay.fmt(s)), f.folder].filter(Boolean).join(' · ')}
	<div style="display:flex;align-items:center;gap:8px;min-width:0;padding-left:21px">
		{#if dense && named}
			<span
				data-selectable
				use:tip={`${f.path}${facts ? '\n' + facts : ''}`}
				style="flex:1;min-width:0;display:flex;gap:8px;align-items:baseline;overflow:hidden;white-space:nowrap"
			>
				<span style="flex:none;font-family:var(--font-mono);font-size:var(--text-sm);color:var(--text-primary)">{f.name}</span>
				<span style="min-width:0;overflow:hidden;text-overflow:ellipsis;font-size:var(--text-xs);color:var(--text-muted)">{fileFacts(f, (s) => timeDisplay.fmt(s))}</span>
			</span>
			<IconBtn title="Show in folder" size={24} onclick={() => show(f.path)}><Icon n="folder-open" s={14} /></IconBtn>
		{:else}
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
					{facts}
				</span>
			</div>
			<Btn variant="ghost" size="sm" icon="folder-open" title={f.path} onclick={() => show(f.path)}>Show in folder</Btn>
		{/if}
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
				{@const reason = reasonLines(o.reason)}
				<div style="padding-left:21px;display:flex;flex-direction:column;gap:4px;min-width:0">
					<div style="display:flex;align-items:flex-start;gap:8px">
						<span
							data-selectable
							style="flex:1;min-width:0;font-size:var(--text-sm);line-height:1.45;color:var(--text-secondary);overflow-wrap:anywhere"
							>{reason.first || 'No reason was given.'}</span
						>
						{#if o.reason}
							<Btn variant="ghost" size="sm" icon="copy" onclick={() => copyText(o.reason, 'Reason')}>Copy</Btn>
						{/if}
					</div>
					{#if reason.rest}
						<!-- A scrolling region has to take focus to be scrolled from the keyboard. -->
						<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
						<pre
							data-selectable
							tabindex="0"
							aria-label="Details of the failure"
							style="margin:0;max-height:{REASON_MAX}px;overflow:auto;padding:4px 6px;border-radius:var(--radius-xs);background:var(--surface-void);font-family:var(--font-mono);font-size:var(--text-xs);line-height:1.4;color:var(--text-secondary);white-space:pre-wrap;overflow-wrap:anywhere">{reason.rest}</pre>
					{/if}
				</div>
			{/if}
		</li>
	{/each}
</ul>
