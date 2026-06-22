<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import { toast } from 'svelte-sonner';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { STATUS_MAP, PRESETS } from './data';
	import type { EditSource, Task, TaskStatus } from '$lib/types';

	const working = $derived(agent.working);
	const disabled = $derived(editor.assets.length === 0);

	let draft = $state('');

	// Most actionable first: the agent's current work and anything awaiting review.
	const RANK: Record<TaskStatus, number> = { working: 0, ready: 1, queued: 2, failed: 3, done: 4 };
	const queue = $derived(
		[...agent.tasks].sort((a, b) => RANK[a.status] - RANK[b.status] || a.created_at.localeCompare(b.created_at))
	);

	// History, newest first.
	const revisions = $derived([...editor.history].reverse());

	const sourceTint: Record<EditSource, string> = {
		agent: 'var(--agent-300)',
		user: 'var(--kerf-400)',
		system: 'var(--text-muted)'
	};
	const sourceIcon: Record<EditSource, string> = {
		agent: 'plug',
		user: 'hand',
		system: 'history'
	};
	const sourceLabel: Record<EditSource, string> = {
		agent: 'Agent',
		user: 'You',
		system: 'Kerf'
	};

	function metaFor(t: Task): string | null {
		if (t.result) return t.result;
		if (t.status === 'queued') return 'waiting · an agent claims it over MCP';
		if (t.status === 'working') return 'agent working over MCP';
		return null;
	}

	async function submit() {
		const v = draft.trim();
		if (disabled || !v) return;
		draft = '';
		try {
			await agent.add(v);
			toast.success('Queued — your connected agent claims tasks over MCP');
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	function onInputKey(e: KeyboardEvent) {
		if (e.key === 'Enter' && !e.shiftKey) {
			e.preventDefault();
			void submit();
		}
	}

	async function runPreset(p: string) {
		const assetId = editor.selectedAssetId ?? editor.assets[0]?.id;
		if (!assetId) {
			toast.error('Import media first');
			return;
		}
		try {
			const task = await agent.add(p);
			// Two presets map to a local op we can run now; the rest wait for the agent.
			if (task && (p === 'Remove silences' || p === 'Assemble rough cut')) {
				if (!editor.analysisFor(assetId)) await ui.runAnalysis(assetId);
				await editor.removeSilence(assetId);
				await agent.resolve(task.id);
				toast.success(p === 'Remove silences' ? 'Removed detected silences' : 'Assembled a rough cut');
			} else {
				toast.info(`Queued “${p}” — your connected agent claims tasks over MCP`);
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	function iconColor(status: TaskStatus): string {
		if (status === 'working') return 'var(--agent-300)';
		if (status === 'ready') return 'var(--green-400)';
		return 'var(--text-muted)';
	}
</script>

{#snippet secHead(label: string, right: string | null)}
	<div style="display:flex;align-items:center;gap:8px;margin-bottom:9px">
		<span
			style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-muted)"
			>{label}</span
		>
		<div style="flex:1;height:1px;background:var(--border-subtle)"></div>
		{#if right}<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">{right}</span>{/if}
	</div>
{/snippet}

{#snippet taskCard(t: Task)}
	{@const s = STATUS_MAP[t.status]}
	{@const meta = metaFor(t)}
	<div
		style="border-radius:var(--radius-md);background:var(--surface-raised);border:1px solid var(--border-default);border-left:{t.status ===
		'ready'
			? '2px solid var(--agent-500)'
			: '1px solid var(--border-default)'};padding:10px 11px"
	>
		<div style="display:flex;align-items:center;gap:8px">
			<Icon n={s.icon} s={13} color={iconColor(t.status)} />
			<span
				style="flex:1;min-width:0;font-size:13px;font-weight:500;color:var(--text-primary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
				title={t.prompt}>{t.prompt}</span
			>
			<Badge tone={s.tone as 'neutral' | 'agent' | 'success'} dot={t.status === 'working'}>{s.label}</Badge>
			{#if t.status !== 'ready'}
				<button
					title="Remove from queue"
					onclick={() => agent.remove(t.id)}
					style="display:inline-flex;align-items:center;justify-content:center;width:20px;height:20px;flex:none;border-radius:var(--radius-sm);border:1px solid transparent;background:transparent;color:var(--text-disabled);cursor:pointer"
				>
					<Icon n="plus" s={13} style="transform:rotate(45deg)" />
				</button>
			{/if}
		</div>
		{#if meta}
			<div style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);margin-top:7px;padding-left:21px">
				{meta}
			</div>
		{/if}
		{#if t.status === 'working'}
			<div style="margin-top:9px;padding-left:21px">
				<div
					style="position:relative;height:5px;border-radius:999px;background:var(--surface-inset);overflow:hidden;border:1px solid var(--border-subtle)"
				>
					<div
						class="kerf-sweep"
						style="position:absolute;top:0;bottom:0;width:30%;background:linear-gradient(90deg, transparent, var(--agent-400), transparent)"
					></div>
				</div>
			</div>
		{/if}
		{#if t.status === 'ready'}
			<div style="display:flex;gap:7px;margin-top:11px;padding-left:21px">
				<button
					onclick={() => agent.resolve(t.id)}
					style="flex:1;height:30px;border-radius:var(--radius-sm);border:1px solid var(--kerf-500);background:var(--kerf-500);color:var(--text-on-accent);font-weight:500;font-size:13px;cursor:pointer"
					>Apply</button
				>
				<button
					onclick={() => agent.remove(t.id)}
					style="flex:1;height:30px;border-radius:var(--radius-sm);border:1px solid var(--border-strong);background:transparent;color:var(--text-secondary);font-size:13px;cursor:pointer"
					>Dismiss</button
				>
			</div>
		{/if}
	</div>
{/snippet}

<div
	style="width:var(--agent-panel-w);flex:none;background:var(--surface-panel);border-left:1px solid var(--border-default);display:flex;flex-direction:column;overflow:hidden"
>
	<div
		style="height:40px;flex:none;display:flex;align-items:center;gap:8px;padding:0 12px;border-bottom:1px solid var(--border-default)"
	>
		<span
			style="width:22px;height:22px;border-radius:var(--radius-sm);background:var(--agent-surface);border:1px solid var(--agent-border);display:grid;place-items:center;color:var(--agent-300)"
			><Icon n="plug" s={13} /></span
		>
		<span style="font:var(--type-title);font-size:14px">Agent queue</span>
		<div style="flex:1"></div>
		<Badge tone={working ? 'agent' : 'neutral'} dot={working}>{working ? 'working' : 'idle'}</Badge>
	</div>

	<div style="flex:1;overflow-y:auto;padding:14px;display:flex;flex-direction:column;gap:16px">
		<!-- MCP status -->
		<div
			style="display:flex;align-items:center;gap:10px;padding:10px 11px;border-radius:var(--radius-md);background:var(--agent-surface);border:1px solid var(--agent-border)"
		>
			<span
				style="flex:none;width:28px;height:28px;border-radius:var(--radius-sm);background:var(--surface-raised);border:1px solid var(--agent-border);display:grid;place-items:center;color:var(--agent-300)"
				><Icon n="plug-zap" s={15} /></span
			>
			<div style="flex:1;min-width:0">
				<div style="display:flex;align-items:center;gap:6px">
					<span style="font-size:13px;font-weight:600;color:var(--text-primary)">Connected agent</span>
					<span
						style="font-family:var(--font-mono);font-size:9px;color:var(--agent-300);letter-spacing:.08em;border:1px solid var(--agent-border);border-radius:3px;padding:0 4px"
						>MCP</span
					>
				</div>
				<div style="font-size:11px;color:var(--text-muted);margin-top:2px">
					Claims tasks over MCP · {working ? 'working a task' : 'idle'}
				</div>
			</div>
			<span
				style="display:inline-flex;align-items:center;gap:5px;font-family:var(--font-mono);font-size:10px;color:{working
					? 'var(--agent-300)'
					: 'var(--green-400)'}"
			>
				<span
					style="width:7px;height:7px;border-radius:50%;background:{working
						? 'var(--agent-400)'
						: 'var(--green-500)'};box-shadow:{working ? '0 0 8px var(--agent-400)' : 'none'}"
				></span>
				{working ? 'working' : 'live'}
			</span>
		</div>

		<!-- queue -->
		<div>
			{@render secHead('Queue', agent.summary)}
			<div style="display:flex;flex-direction:column;gap:8px">
				{#if queue.length === 0}
					<div
						style="display:flex;flex-direction:column;align-items:center;gap:7px;padding:22px 16px;border-radius:var(--radius-md);border:1px dashed var(--border-strong);background:var(--surface-inset);text-align:center"
					>
						<Icon n="list-plus" s={20} color="var(--text-disabled)" />
						<div style="font-size:12px;color:var(--text-secondary)">No tasks queued</div>
						<div style="font-size:11px;color:var(--text-muted);line-height:1.5">
							Queue a task below. Your connected agent claims it and proposes edits.
						</div>
					</div>
				{:else}
					{#each queue as t (t.id)}
						{@render taskCard(t)}
					{/each}
				{/if}
			</div>
		</div>

		<!-- history -->
		<div>
			{@render secHead('History', `${editor.history.length} edit${editor.history.length === 1 ? '' : 's'}`)}
			<div style="display:flex;flex-direction:column;gap:1px">
				{#each revisions as rev (rev.seq)}
					<div
						style="display:flex;gap:9px;align-items:center;padding:6px 4px;border-radius:var(--radius-sm);{rev.current
							? 'background:var(--surface-raised)'
							: ''}"
					>
						<Icon n={sourceIcon[rev.source]} s={13} color={sourceTint[rev.source]} style="flex:none" />
						<div style="flex:1;min-width:0">
							<div
								style="font-size:12px;line-height:1.35;color:var(--text-secondary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
							>
								{rev.label}
							</div>
							<div style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">
								{sourceLabel[rev.source]}
							</div>
						</div>
						{#if rev.current}
							<span
								style="display:inline-flex;align-items:center;gap:4px;font-family:var(--font-mono);font-size:9px;color:var(--green-400)"
							>
								<span style="width:6px;height:6px;border-radius:50%;background:var(--green-500)"></span>now
							</span>
						{:else}
							<button
								title="Revert the timeline to this point"
								onclick={() => editor.revertTo(rev.seq)}
								style="display:inline-flex;align-items:center;gap:4px;padding:2px 7px;border-radius:var(--radius-full);background:var(--surface-inset);border:1px solid var(--border-strong);color:var(--text-secondary);font-size:10px;cursor:pointer"
							>
								<Icon n="rotate-ccw" s={11} />Revert
							</button>
						{/if}
					</div>
				{/each}
			</div>
		</div>
	</div>

	<!-- add task -->
	<div
		style="flex:none;padding:12px;border-top:1px solid var(--border-default);background:var(--surface-app);display:flex;flex-direction:column;gap:9px"
	>
		<div style="display:flex;flex-wrap:wrap;gap:6px">
			{#each PRESETS as p (p)}
				<button
					{disabled}
					onclick={() => runPreset(p)}
					style="display:inline-flex;align-items:center;gap:5px;padding:5px 9px;border-radius:var(--radius-full);background:var(--surface-inset);border:1px solid var(--border-strong);color:{disabled
						? 'var(--text-disabled)'
						: 'var(--text-secondary)'};font-size:11px;cursor:{disabled ? 'not-allowed' : 'pointer'}"
				>
					<Icon n="plus" s={12} />{p}
				</button>
			{/each}
		</div>
		<div
			style="display:flex;align-items:center;gap:8px;height:36px;padding:0 10px;background:var(--surface-inset);border:1px solid var(--input);border-radius:var(--radius-sm);opacity:{disabled
				? 0.5
				: 1}"
		>
			<Icon n="list-plus" s={14} color="var(--text-muted)" />
			<input
				{disabled}
				bind:value={draft}
				onkeydown={onInputKey}
				placeholder="Describe a task to queue…"
				style="flex:1;background:none;border:none;outline:none;color:var(--text-primary);font-family:var(--font-sans);font-size:13px"
			/>
			<button
				title="Add to queue"
				disabled={disabled || !draft.trim()}
				onclick={submit}
				style="display:inline-flex;align-items:center;justify-content:center;width:24px;height:24px;border-radius:var(--radius-sm);border:1px solid transparent;background:transparent;color:{draft.trim()
					? 'var(--kerf-300)'
					: 'var(--text-secondary)'};cursor:{disabled || !draft.trim() ? 'not-allowed' : 'pointer'}"
			>
				<Icon n="corner-down-left" s={14} />
			</button>
		</div>
		<span style="font-size:10px;color:var(--text-disabled);line-height:1.4">
			Tasks run when your connected agent claims them — Kerf never edits on its own.
		</span>
	</div>
</div>
