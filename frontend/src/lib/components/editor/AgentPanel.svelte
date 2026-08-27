<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import { toast } from 'svelte-sonner';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { mcpEndpoint } from '$lib/api';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import { diffHeadline, groupEntries, polarity } from '$lib/diff';
	import { STATUS_MAP, PRESETS } from './data';
	import type { DiffEntry, EditSource, Task, TaskStatus, TimelineDiff } from '$lib/types';

	const working = $derived(agent.working);
	const disabled = $derived(editor.assets.length === 0);

	let draft = $state('');

	// How to connect an agent: the local MCP endpoint + a ready-to-run Claude Code
	// command. Loaded from the backend so the displayed URL honors KERF_MCP_ADDR.
	let endpoint = $state('http://127.0.0.1:7777/mcp');
	let showConnect = $state(true);
	let copied = $state<string | null>(null);
	const claudeCmd = $derived(`claude mcp add --transport http kerf ${endpoint}`);

	$effect(() => {
		mcpEndpoint().then((e) => (endpoint = e)).catch(() => {});
	});

	async function copy(text: string, key: string) {
		try {
			await navigator.clipboard.writeText(text);
			copied = key;
			setTimeout(() => {
				if (copied === key) copied = null;
			}, 1400);
		} catch {
			toast.error('Could not copy — select the text and copy manually');
		}
	}

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

	// ---- the agent's pending proposal ---------------------------------------
	const staged = $derived(editor.staged);
	const headline = $derived(staged ? diffHeadline(staged.diff) : '');
	const groups = $derived(staged ? groupEntries(staged.diff.entries) : []);
	let applying = $state(false);

	// The design system already names these three; a proposal is a diff.
	const polarityTint: Record<'added' | 'removed' | 'changed', string> = {
		added: 'var(--diff-add)',
		removed: 'var(--diff-remove)',
		changed: 'var(--diff-shift)'
	};
	const polarityMark: Record<'added' | 'removed' | 'changed', string> = {
		added: '+',
		removed: '−',
		changed: '~'
	};

	/** Jump the playhead to where a change happens, so it can be looked at. */
	function inspect(e: DiffEntry) {
		if (e.at == null) return;
		ui.seek(e.at);
		if (e.clip_id) editor.selectClip(e.clip_id);
	}

	async function applyProposal() {
		if (!staged) return;
		if (staged.stale && !confirm('You have edited the timeline since these changes were staged. Applying replaces your newer cut. Continue?'))
			return;
		applying = true;
		try {
			await editor.applyStaged(staged.stale);
			toast.success('Applied the proposed changes');
		} catch (err) {
			toast.error(err instanceof Error ? err.message : String(err));
		} finally {
			applying = false;
		}
	}

	async function discardProposal() {
		try {
			await editor.discardStaged();
			toast.info('Discarded the proposed changes');
		} catch (err) {
			toast.error(err instanceof Error ? err.message : String(err));
		}
	}

	// ---- "what did that edit change?" --------------------------------------
	// Same diff engine, pointed at the edit log: a revision label says which
	// operation ran, not what it did to the cut.
	let openRevision = $state<number | null>(null);
	let revisionDiffs = $state<Record<number, TimelineDiff | null>>({});

	async function toggleRevision(seq: number) {
		if (openRevision === seq) {
			openRevision = null;
			return;
		}
		openRevision = seq;
		if (!(seq in revisionDiffs)) {
			try {
				revisionDiffs[seq] = await editor.revisionDiff(seq);
			} catch {
				revisionDiffs[seq] = null;
			}
		}
	}

	async function togglePreview() {
		if (editor.previewingStaged) await editor.exitStagedPreview();
		else await editor.previewStaged();
	}

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

	/** The video cuts as they stand, to tell a no-op alignment from a real one. */
	function cutSignature(): string {
		return JSON.stringify(
			editor.timeline.tracks
				.filter((t) => t.kind === 'video')
				.map((t) => t.clips.map((c) => [c.timeline_start, c.source_in, c.source_out]))
		);
	}

	async function runPreset(p: string) {
		const assetId = editor.selectedAssetId ?? editor.assets[0]?.id;
		if (!assetId) {
			toast.error('Import media first');
			return;
		}
		try {
			const task = await agent.add(p);
			// Three presets map to a local op we can run now; the rest wait for the agent.
			if (task && (p === 'Remove silences' || p === 'Assemble rough cut')) {
				if (!editor.analysisFor(assetId)) await ui.runAnalysis(assetId);
				await editor.removeSilence(assetId);
				await agent.resolve(task.id);
				toast.success(p === 'Remove silences' ? 'Removed detected silences' : 'Assembled a rough cut');
			} else if (task && p === 'Cut to the beat') {
				// The grid comes from the music, so analyze whatever is on the audio tracks.
				const music = [
					...new Set(
						editor.timeline.tracks.filter((t) => t.kind === 'audio').flatMap((t) => t.clips.map((c) => c.asset_id))
					)
				];
				if (music.length === 0) throw new Error('Put music on an audio track first');
				for (const id of music) if (!editor.analysisFor(id)) await ui.runAnalysis(id);
				const before = cutSignature();
				await editor.snapToBeats();
				await agent.resolve(task.id);
				// A grid that does not reach the cuts leaves them alone; say so
				// rather than claiming an alignment that never happened.
				if (cutSignature() === before) toast.info('No cuts were near a beat');
				else toast.success('Aligned the cuts to the beat');
			} else if (task && p === 'Frame for the delivery') {
				// Smart crop only matters once the project has a frame to be cut
				// for; without one the frame follows the footage and every shot
				// already fills it.
				await editor.smartCrop();
				await agent.resolve(task.id);
				toast.success('Framed every shot for the delivery frame', {
					action: { label: 'Undo', onClick: () => void editor.undo() }
				});
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

	// Right-click on the panel (outside text fields): copy the connection details
	// and quick-queue a preset task.
	function onAgentContextMenu(e: MouseEvent) {
		const t = e.target as Element | null;
		if (t?.closest('input, textarea, [contenteditable="true"], [data-selectable]')) return;
		const items: MenuItem[] = [
			{ label: 'Copy MCP endpoint', icon: 'copy', action: () => void copy(endpoint, 'endpoint') },
			{ label: 'Copy connect command', icon: 'copy', action: () => void copy(claudeCmd, 'cmd') },
			{ type: 'separator' },
			...PRESETS.map(
				(p): MenuItem => ({ label: `Queue: ${p}`, icon: 'list-plus', disabled, action: () => void runPreset(p) })
			)
		];
		contextMenu.show(e, items);
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

{#snippet copyRow(value: string, key: string)}
	<div
		style="display:flex;align-items:center;gap:6px;background:var(--surface-inset);border:1px solid var(--border-subtle);border-radius:var(--radius-sm);padding:6px 6px 6px 9px"
	>
		<code
			data-selectable
			style="flex:1;min-width:0;font-family:var(--font-mono);font-size:11px;color:var(--text-secondary);white-space:nowrap;overflow-x:auto"
			>{value}</code
		>
		<button
			title="Copy to clipboard"
			onclick={() => copy(value, key)}
			style="display:inline-flex;align-items:center;justify-content:center;width:24px;height:24px;flex:none;border-radius:var(--radius-sm);border:1px solid var(--border-strong);background:var(--surface-raised);color:{copied ===
			key
				? 'var(--green-400)'
				: 'var(--text-secondary)'};cursor:pointer"
		>
			<Icon n={copied === key ? 'check' : 'copy'} s={13} />
		</button>
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
			<div data-selectable style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted);margin-top:7px;padding-left:21px">
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
			{#if staged?.task_id === t.id}
				<div style="font-size:11px;color:var(--agent-300);margin-top:8px;padding-left:21px">
					{staged.diff.entries.length} proposed change{staged.diff.entries.length === 1 ? '' : 's'} below — applying accepts them
				</div>
			{/if}
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
	role="presentation"
	oncontextmenu={onAgentContextMenu}
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
			style="flex:none;display:flex;align-items:center;gap:10px;padding:10px 11px;border-radius:var(--radius-md);background:var(--agent-surface);border:1px solid var(--agent-border)"
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

		<!-- the agent's pending proposal -->
		{#if staged}
			<div
				style="flex:none;border-radius:var(--radius-md);background:var(--surface-raised);border:1px solid var(--agent-border);border-left:2px solid var(--agent-500);overflow:hidden"
			>
				<div style="display:flex;align-items:center;gap:8px;padding:10px 11px 0">
					<Icon n="git-pull-request-arrow" s={14} color="var(--agent-300)" />
					<span style="flex:1;font-size:13px;font-weight:600;color:var(--text-primary)">Proposed changes</span>
					<Badge tone="agent">review</Badge>
				</div>
				<div style="padding:7px 11px 0 32px">
					{#if staged.note}
						<div style="font-size:12px;color:var(--text-secondary);margin-bottom:3px">{staged.note}</div>
					{/if}
					<div style="font-family:var(--font-mono);font-size:11px;color:var(--text-muted)">{headline}</div>
					<div style="font-size:10px;color:var(--text-disabled);margin-top:3px">
						Your timeline is untouched until you apply these.
					</div>
				</div>

				{#if staged.stale}
					<div
						style="margin:9px 11px 0;padding:7px 9px;border-radius:var(--radius-sm);background:var(--diff-remove-surface);border:1px solid var(--diff-remove);font-size:11px;color:var(--text-secondary);line-height:1.45"
					>
						You have edited the timeline since these were staged — applying replaces your newer cut.
					</div>
				{/if}

				<div style="padding:10px 11px 0 32px;display:flex;flex-direction:column;gap:9px">
					{#each groups as group (group.label)}
						<div>
							<div
								style="font:var(--type-overline);letter-spacing:var(--tracking-caps);text-transform:uppercase;color:var(--text-disabled);margin-bottom:4px"
							>
								{group.label}
							</div>
							<div style="display:flex;flex-direction:column;gap:2px">
								{#each group.entries as e, i (`${e.kind}-${i}-${e.summary}`)}
									{@const tone = polarity(e.kind)}
									<button
										onclick={() => inspect(e)}
										disabled={e.at == null}
										title={e.at == null ? e.summary : `Jump to ${e.summary}`}
										style="display:flex;gap:7px;align-items:baseline;width:100%;text-align:left;padding:3px 4px;border:none;border-radius:var(--radius-sm);background:transparent;color:inherit;cursor:{e.at ==
										null
											? 'default'
											: 'pointer'}"
									>
										<span
											style="flex:none;width:9px;font-family:var(--font-mono);font-size:11px;color:{polarityTint[tone]}"
											>{polarityMark[tone]}</span
										>
										<span style="flex:1;min-width:0;font-size:11px;line-height:1.45;color:var(--text-secondary)">
											{e.summary}
											{#if e.detail}<span style="color:var(--text-muted)"> · {e.detail}</span>{/if}
										</span>
									</button>
								{/each}
							</div>
						</div>
					{/each}
				</div>

				<div style="display:flex;gap:7px;padding:12px 11px">
					<button
						onclick={applyProposal}
						disabled={applying}
						style="flex:1;height:30px;border-radius:var(--radius-sm);border:1px solid var(--kerf-500);background:var(--kerf-500);color:var(--text-on-accent);font-weight:500;font-size:13px;cursor:pointer"
						>{applying ? 'Applying…' : 'Apply'}</button
					>
					<button
						onclick={togglePreview}
						title="Show the proposed cut in the editor"
						style="height:30px;padding:0 10px;border-radius:var(--radius-sm);border:1px solid {editor.previewingStaged
							? 'var(--agent-500)'
							: 'var(--border-strong)'};background:{editor.previewingStaged
							? 'var(--agent-surface)'
							: 'transparent'};color:{editor.previewingStaged
							? 'var(--agent-300)'
							: 'var(--text-secondary)'};font-size:13px;cursor:pointer;display:inline-flex;align-items:center;gap:5px"
					>
						<Icon n={editor.previewingStaged ? 'eye-off' : 'eye'} s={13} />
						{editor.previewingStaged ? 'Exit' : 'Preview'}
					</button>
					<button
						onclick={discardProposal}
						style="height:30px;padding:0 10px;border-radius:var(--radius-sm);border:1px solid var(--border-strong);background:transparent;color:var(--text-secondary);font-size:13px;cursor:pointer"
						>Discard</button
					>
				</div>
			</div>
		{/if}

		<!-- how to connect an agent -->
		<div
			style="flex:none;border-radius:var(--radius-md);background:var(--surface-raised);border:1px solid var(--border-default);overflow:hidden"
		>
			<button
				onclick={() => (showConnect = !showConnect)}
				style="display:flex;align-items:center;gap:8px;width:100%;padding:10px 11px;background:transparent;border:none;cursor:pointer;text-align:left"
			>
				<Icon n="plug" s={13} color="var(--agent-300)" />
				<span style="flex:1;font-size:12px;font-weight:600;color:var(--text-primary)">Connect an agent</span>
				<Icon
					n="chevron-down"
					s={15}
					color="var(--text-muted)"
					style="transition:transform .15s;transform:rotate({showConnect ? 180 : 0}deg)"
				/>
			</button>
			{#if showConnect}
				<div style="padding:0 11px 12px;display:flex;flex-direction:column;gap:11px">
					<div>
						<div style="font-size:11px;color:var(--text-muted);line-height:1.5;margin-bottom:6px">
							Point any MCP client at this local endpoint:
						</div>
						{@render copyRow(endpoint, 'endpoint')}
					</div>
					<div>
						<div style="font-size:11px;color:var(--text-muted);line-height:1.5;margin-bottom:6px">
							Using <span style="color:var(--text-secondary)">Claude Code</span>? Run this in your terminal:
						</div>
						{@render copyRow(claudeCmd, 'cmd')}
					</div>
					<div style="font-size:10px;color:var(--text-disabled);line-height:1.5">
						The agent edits the project you have open and proposes cuts you review here. Override the address
						with <span style="font-family:var(--font-mono)">KERF_MCP_ADDR</span>.
					</div>
				</div>
			{/if}
		</div>

		<!-- queue -->
		<div style="flex:none">
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
		<div style="flex:none">
			{@render secHead('History', `${editor.history.length} edit${editor.history.length === 1 ? '' : 's'}`)}
			<div style="display:flex;flex-direction:column;gap:1px">
				{#each revisions as rev (rev.seq)}
					<div
						style="display:flex;gap:9px;align-items:center;padding:6px 4px;border-radius:var(--radius-sm);{rev.current
							? 'background:var(--surface-raised)'
							: ''}"
					>
						<Icon n={sourceIcon[rev.source]} s={13} color={sourceTint[rev.source]} style="flex:none" />
						<button
							onclick={() => toggleRevision(rev.seq)}
							title="What this edit changed"
							style="flex:1;min-width:0;text-align:left;background:none;border:none;padding:0;cursor:pointer"
						>
							<div
								style="font-size:12px;line-height:1.35;color:var(--text-secondary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
							>
								{rev.label}
							</div>
							<div style="font-family:var(--font-mono);font-size:10px;color:var(--text-disabled)">
								{sourceLabel[rev.source]}
							</div>
						</button>
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
					{#if openRevision === rev.seq}
						{@const d = revisionDiffs[rev.seq]}
						<div style="padding:2px 4px 8px 26px;display:flex;flex-direction:column;gap:2px">
							{#if d === undefined}
								<span style="font-size:11px;color:var(--text-disabled)">Reading the change…</span>
							{:else if d === null}
								<span style="font-size:11px;color:var(--text-disabled)"
									>Change details are available in the desktop app.</span
								>
							{:else if d.entries.length === 0}
								<span style="font-size:11px;color:var(--text-disabled)">Changed nothing.</span>
							{:else}
								<span style="font-family:var(--font-mono);font-size:10px;color:var(--text-muted)"
									>{diffHeadline(d)}</span
								>
								{#each d.entries as e, i (`${rev.seq}-${i}`)}
									<span style="display:flex;gap:6px;align-items:baseline">
										<span
											style="flex:none;width:9px;font-family:var(--font-mono);font-size:11px;color:{polarityTint[
												polarity(e.kind)
											]}">{polarityMark[polarity(e.kind)]}</span
										>
										<span style="flex:1;min-width:0;font-size:11px;line-height:1.45;color:var(--text-secondary)"
											>{e.summary}{#if e.detail}<span style="color:var(--text-muted)"> · {e.detail}</span>{/if}</span
										>
									</span>
								{/each}
							{/if}
						</div>
					{/if}
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
