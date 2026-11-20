<script lang="ts">
	// The Proxy queue panel (dock id `proxyQueue`): the preview proxies being built and waiting, in the order they run, each
	// with when it should be done, and the way to steer them — build one next, cancel one, pause the lot, cancel all, and
	// rebuild one that failed. It exists because, with Proxy only on, the preview waits for a proxy and said only that it
	// waited: how long, behind what, and which one mattered were out of sight.
	//
	// What it shows is `proxyQueue` (`proxy-queue.svelte.ts`), read while this panel is on screen and not otherwise; every
	// word and number is `proxy-queue.ts` (pure, bun-tested). A queue of hundreds draws a window of it (`ROW_CAP`, "Show
	// more"): the rows are keyed by asset, so a job that moves keeps its element (focus does not survive the move, and is put
	// back after a key press).
	import { tick } from 'svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import EmptyState from './EmptyState.svelte';
	import Icon from './Icon.svelte';
	import IconBtn from './IconBtn.svelte';
	import ProgressBar from './ProgressBar.svelte';
	import { confirmAction } from '#lib/api';
	import { cancelProxyQueueCopy } from '#lib/confirm-copy';
	import { report } from '#lib/ops';
	import { ROW_CAP, failedRows, pipelineHint, pipelineLabel, queueRows, summarize } from '#lib/proxy-queue';
	import { proxyQueue } from '#lib/proxy-queue.svelte';
	import { intersectionObserverFor } from '#lib/realm';
	import { tip } from '#lib/tip';
	import { windows } from '#lib/windows.svelte';

	let root = $state<HTMLDivElement | null>(null);

	// Read the queue only while this panel is on screen: a tab behind another, or a hidden window, asks for nothing.
	let visible = $state(true);
	$effect(() => {
		if (!root) return;
		void windows.version;
		const io = intersectionObserverFor(root, (entries) => (visible = entries.at(-1)?.isIntersecting ?? true));
		io.observe(root);
		return () => io.disconnect();
	});
	$effect(() => {
		if (!visible) return;
		return proxyQueue.watch();
	});

	const queue = $derived(proxyQueue.queue);
	const readAt = $derived(new Date(proxyQueue.readAt));
	const summary = $derived(summarize(queue, readAt));
	let cap = $state(ROW_CAP);
	const list = $derived(queueRows(queue, readAt, cap));
	const failed = $derived(failedRows(queue));
	const jobs = $derived(queue?.jobs.length ?? 0);

	/** A pointer click does not leave focus on the button: a focused button swallows Space, and the transport's key would
	 *  stop until something else was clicked. Enter and Space on it (`detail` 0) keep it. */
	function release(e: MouseEvent) {
		if (e.detail > 0) (e.currentTarget as HTMLElement | null)?.blur();
	}

	/** Run a call about the queue; what the backend refuses is said as a notice. */
	async function run(op: () => Promise<void>) {
		try {
			await op();
		} catch (e) {
			report(e);
		}
	}

	/** Where focus goes when the row it was on has gone: the row now in its place, else the one before it, else the
	 *  panel itself — not to the page, where the next key would be a shortcut. */
	async function refocus(index: number) {
		await tick();
		const rows = root?.querySelectorAll<HTMLElement>('[data-row]') ?? [];
		const next = rows[Math.min(index, rows.length - 1)];
		(next?.querySelector<HTMLElement>('button') ?? root)?.focus();
	}

	/** A row that moved lost focus with its place in the document: put it back on the job (its Cancel, now that Build next
	 *  is not offered to the first one waiting). */
	async function refocusJob(id: string) {
		await tick();
		const row = root?.querySelector<HTMLElement>(`[data-row][data-asset="${CSS.escape(id)}"]`);
		(row?.querySelector<HTMLElement>('button') ?? root)?.focus();
	}

	function build(e: MouseEvent, id: string) {
		const keyboard = e.detail === 0;
		release(e);
		void run(() => proxyQueue.prioritize(id)).then(() => (keyboard ? refocusJob(id) : undefined));
	}

	function cancel(e: MouseEvent, id: string, index: number) {
		const keyboard = e.detail === 0;
		release(e);
		void run(() => proxyQueue.cancel(id)).then(() => (keyboard ? refocus(index) : undefined));
	}

	function rebuild(e: MouseEvent, id: string) {
		release(e);
		void run(() => proxyQueue.rebuild(id));
	}

	function pause(e: MouseEvent) {
		release(e);
		void run(() => proxyQueue.setPaused(!queue?.paused));
	}

	async function cancelAll(e: MouseEvent) {
		const keyboard = e.detail === 0;
		release(e);
		if (jobs === 0) return;
		const copy = cancelProxyQueueCopy(jobs);
		if (!(await confirmAction(copy.message, copy.title, copy))) return;
		await run(() => proxyQueue.cancelAll());
		// The button is off with nothing left to cancel, and a button that goes off drops focus: keep it in the panel.
		if (keyboard) {
			await tick();
			root?.focus();
		}
	}
</script>

<div class="pq" bind:this={root} tabindex="-1" data-testid="proxy-queue">
	<header class="head">
		<div class="lead">
			<div class="count" data-testid="proxy-queue-count">{summary.count}</div>
			{#if summary.time}<div class="time" data-testid="proxy-queue-time">{summary.time}</div>{/if}
		</div>
		{#if queue}
			<span class="pipeline" use:tip={pipelineHint(queue.pipeline)}>
				Encoder <Badge>{pipelineLabel(queue.pipeline)}</Badge>
			</span>
		{/if}
		<div class="actions">
			<Btn
				size="sm"
				icon={queue?.paused ? 'play' : 'pause'}
				iconSize={13}
				pending={proxyQueue.working}
				disabled={!queue || (jobs === 0 && !queue.paused)}
				title={queue?.paused ? 'Start building the proxies again' : jobs === 0 ? 'Nothing is queued' : 'Let the build under way finish and start nothing new'}
				data-testid="proxy-queue-pause"
				onclick={pause}>{queue?.paused ? 'Resume' : 'Pause'}</Btn
			>
			<Btn
				size="sm"
				variant="destructive"
				data-quiet
				icon="x"
				iconSize={13}
				pending={proxyQueue.working}
				disabled={jobs === 0}
				title={jobs === 0 ? 'Nothing is queued' : 'Stop building and forget every proxy in the queue'}
				data-testid="proxy-queue-cancel-all"
				onclick={cancelAll}>Cancel all</Btn
			>
		</div>
		<div class="status" role="status">
			{#if summary.note}
				<Icon n={summary.tone === 'waiting' ? 'clock' : 'pause'} s={12} />
				<span data-testid="proxy-queue-note">{summary.note}</span>
			{/if}
		</div>
	</header>

	{#if proxyQueue.error}
		<p class="error" role="alert">
			<Icon n="alert-triangle" s={13} color="var(--danger)" />
			<span>Couldn't read the proxy queue — {proxyQueue.error}</span>
			<Btn size="sm" variant="ghost" onclick={(e: MouseEvent) => (release(e), void proxyQueue.refresh())}>Try again</Btn>
		</p>
	{/if}

	<div class="body">
		{#if queue && jobs === 0}
			<EmptyState
				icon="package-check"
				title="Nothing to wait for"
				hint="The preview proxies of new footage are queued here as it is imported, and as you rebuild or ask for one."
				pad={failed.length > 0 ? 16 : 36}
			/>
		{/if}

		{#if list.rows.length > 0}
			<ol class="rows" role="list" aria-label="Preview proxies, in the order they are built">
				{#each list.rows as row, i (row.id)}
					<li class="row" data-row data-state={row.state} data-testid="proxy-row" data-asset={row.id}>
						<span class="pos" aria-hidden="true">{row.position}</span>
						<div class="main">
							<div class="name" use:tip={row.name}>{row.name}</div>
							<div class="line">
								<span class="meta">{row.meta}</span>
								{#if row.eta}<span class="eta" use:tip={row.etaTitle} data-testid="proxy-row-eta">{row.eta}</span>{/if}
							</div>
							{#if row.state === 'building'}
								<ProgressBar value={row.fraction} label="Building the proxy of {row.name}" height={4} />
								<div class="progress" data-testid="proxy-row-progress">{row.progress}</div>
							{/if}
						</div>
						<div class="acts">
							{#if row.canPrioritize}
								<IconBtn
									size={24}
									title={{ text: 'Build next', detail: 'Moves it in front of the proxies that are waiting.' }}
									aria-label="Build the proxy of {row.name} next"
									pending={proxyQueue.busy.has(row.id)}
									data-testid="proxy-row-next"
									onclick={(e: MouseEvent) => build(e, row.id)}><Icon n="arrow-up" s={13} /></IconBtn
								>
							{/if}
							<IconBtn
								size={24}
								title={row.state === 'building' ? 'Cancel this build' : 'Cancel this proxy'}
								aria-label="Cancel the proxy of {row.name}"
								pending={proxyQueue.busy.has(row.id)}
								data-testid="proxy-row-cancel"
								onclick={(e: MouseEvent) => cancel(e, row.id, i)}><Icon n="x" s={13} /></IconBtn
							>
						</div>
					</li>
				{/each}
			</ol>
			{#if list.more > 0}
				<div class="more" data-testid="proxy-queue-more">
					<span>and {list.more} more</span>
					<Btn size="sm" variant="ghost" onclick={(e: MouseEvent) => (release(e), (cap += ROW_CAP))}
						>Show {Math.min(list.more, ROW_CAP)} more</Btn
					>
				</div>
			{/if}
		{/if}

		{#if failed.length > 0}
			<h3 class="section">Failed ({failed.length})</h3>
			<ul class="rows" role="list" aria-label="Preview proxies that failed">
				{#each failed as f (f.id)}
					<li class="row failed" data-testid="proxy-failed" data-asset={f.id}>
						<span class="pos" aria-hidden="true"><Icon n="alert-triangle" s={13} color="var(--danger)" /></span>
						<div class="main">
							<div class="name" use:tip={f.name}>{f.name}</div>
							<div class="reason" data-selectable>{f.reason}</div>
						</div>
						<div class="acts">
							<Btn
								size="sm"
								icon="refresh-cw"
								iconSize={12}
								aria-label="Rebuild the proxy of {f.name}"
								pending={proxyQueue.busy.has(f.id)}
								data-testid="proxy-failed-rebuild"
								onclick={(e: MouseEvent) => rebuild(e, f.id)}>Rebuild</Btn
							>
						</div>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
</div>

<style>
	.pq {
		flex: 1;
		min-height: 0;
		min-width: 0;
		display: flex;
		flex-direction: column;
		box-sizing: border-box;
		background: var(--surface-panel);
		outline: none;
	}
	.head {
		flex: none;
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 6px 10px;
		padding: 10px 10px 8px;
		border-bottom: var(--line-width) solid var(--border-subtle);
	}
	.lead {
		flex: 1 1 140px;
		min-width: 0;
	}
	.count {
		font: var(--type-ui);
		font-weight: 600;
		color: var(--text-primary);
	}
	.time {
		margin-top: 2px;
		font-size: var(--text-xs);
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}
	.pipeline {
		flex: none;
		display: inline-flex;
		align-items: center;
		gap: 6px;
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	.actions {
		flex: 1 1 100%;
		display: flex;
		flex-wrap: wrap;
		gap: 6px;
	}
	.status {
		flex: 1 1 100%;
		display: flex;
		align-items: flex-start;
		gap: 6px;
		min-height: 0;
		font-size: var(--text-xs);
		line-height: 1.4;
		color: var(--text-secondary);
	}
	.status:empty {
		display: none;
	}
	.status :global(svg) {
		margin-top: 2px;
		color: var(--warning);
	}
	.error {
		flex: none;
		display: flex;
		align-items: center;
		flex-wrap: wrap;
		gap: 6px;
		margin: 0;
		padding: 8px 10px;
		font-size: var(--text-xs);
		color: var(--text-primary);
		background: var(--danger-surface);
	}
	.body {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		padding: 8px 10px 12px;
	}
	.rows {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 6px;
	}
	.row {
		display: grid;
		grid-template-columns: 22px minmax(0, 1fr) auto;
		align-items: start;
		gap: 8px;
		padding: 7px 6px 7px 8px;
		border-radius: var(--radius-sm);
		background: var(--surface-raised);
		border: var(--line-width) solid var(--border-subtle);
	}
	.row[data-state='building'] {
		border-color: var(--agent-border);
	}
	.row.failed {
		border-color: color-mix(in srgb, var(--danger) 40%, transparent);
	}
	.pos {
		padding-top: 1px;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		font-variant-numeric: tabular-nums;
		color: var(--text-muted);
		text-align: right;
	}
	.main {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 3px;
	}
	.name {
		font-size: var(--text-sm);
		font-weight: 500;
		color: var(--text-primary);
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}
	.line {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		justify-content: space-between;
		gap: 0 8px;
		font-size: var(--text-xs);
		color: var(--text-muted);
		font-variant-numeric: tabular-nums;
	}
	.eta {
		margin-left: auto;
		color: var(--text-secondary);
		white-space: nowrap;
	}
	.progress {
		font-size: var(--text-xs);
		color: var(--text-secondary);
		font-variant-numeric: tabular-nums;
	}
	.reason {
		font-size: var(--text-xs);
		line-height: 1.4;
		color: var(--text-secondary);
		overflow-wrap: anywhere;
		display: -webkit-box;
		-webkit-line-clamp: 3;
		line-clamp: 3;
		-webkit-box-orient: vertical;
		overflow: hidden;
	}
	/* Room for both buttons whether the row has one or two, so the times above them line up down the list. */
	.acts {
		box-sizing: border-box;
		width: 52px;
		display: flex;
		align-items: center;
		justify-content: flex-end;
		gap: 2px;
	}
	.failed .acts {
		width: auto;
	}
	.more {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 8px;
		padding: 6px 4px 0;
		font-size: var(--text-xs);
		color: var(--text-muted);
	}
	.section {
		margin: 14px 0 6px;
		font: var(--type-overline);
		letter-spacing: var(--tracking-caps);
		text-transform: uppercase;
		color: var(--text-muted);
	}
</style>
