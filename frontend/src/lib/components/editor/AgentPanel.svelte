<script lang="ts">
	import Icon from './Icon.svelte';
	import Badge from './Badge.svelte';
	import Btn from './Btn.svelte';
	import IconBtn from './IconBtn.svelte';
	import { toast } from '$lib/notifications.svelte';
	import { ui } from '$lib/editor-ui.svelte';
	import { editor } from '$lib/state.svelte';
	import { agent } from '$lib/agent.svelte';
	import { dismissTask, readyCard, resolveReady } from '$lib/agent-tasks';
	import { agentStatus } from '$lib/api';
	import { contextMenu } from '$lib/context-menu.svelte';
	import type { MenuItem } from '$lib/context-menu.svelte';
	import { diffHeadline, groupEntries, polarity } from '$lib/diff';
	import { confirmReplaceCaptions } from '$lib/title-actions';
	import { QUICK_EDIT_STEPS } from '$lib/analysis-steps';
	import { applyProposal, discardProposal, inspectChange } from '$lib/proposal-actions';
	import { cutSignature, isLocalPreset, quickTarget, silenceOutcome, targetNote } from '$lib/quick-edits';
	import { STATUS_MAP, PRESETS } from './data';
	import { agentLine, showComposerHint } from '$lib/agent-status';
	import type { DiffEntry, EditSource, Task, TaskStatus, TimelineDiff } from '$lib/types';
	import { tip } from '$lib/tip';

	const working = $derived(agent.working);
	const disabled = $derived(editor.assets.length === 0 || editor.busy);
	/** The footage the one-asset quick edits act on, said beside them (`quick-edits.ts`). */
	const target = $derived(
		quickTarget({
			clipAssetId: editor.selectedClip?.asset_id,
			assetId: editor.selectedAssetId,
			assets: editor.assets,
			timeline: editor.liveTimeline
		})
	);

	let draft = $state('');

	// How to connect an agent: the local MCP endpoint + a ready-to-run Claude Code
	// command. Loaded from the backend so the displayed URL honors KERF_MCP_ADDR.
	let endpoint = $state('http://127.0.0.1:7777/mcp');
	let showConnect = $state(false);
	let connectEl = $state<HTMLElement | null>(null);
	let quickOpen = $state(true);
	/** The panel's own height: below ~520 px there is no room for the composer's hint line. */
	let panelHeight = $state(0);
	let copied = $state<string | null>(null);
	const claudeCmd = $derived(`claude mcp add --transport http kerf ${endpoint}`);

	// Seconds since an agent last spoke to the endpoint, or null if none ever
	// has. A streamable-HTTP client holds no connection between calls, so there
	// is no socket to watch — this panel used to show a green "live" dot whether
	// or not anything was on the other end, which is the one thing it must not
	// say. Polled, because an agent arriving is not an event the app hears.
	let lastSeen = $state<number | null>(null);
	/** Why the endpoint is not listening (port taken), or null while it is. */
	let serverError = $state<string | null>(null);
	$effect(() => {
		let stop = false;
		const poll = async () => {
			try {
				const s = await agentStatus();
				if (stop) return;
				endpoint = s.endpoint;
				lastSeen = s.last_seen_secs;
				serverError = s.error;
			} catch {
				/* the endpoint is part of the app; a failed read is not worth a toast */
			}
		};
		void poll();
		const t = setInterval(() => void poll(), 5000);
		return () => {
			stop = true;
			clearInterval(t);
		};
	});

	/** An agent counts as connected while it has spoken recently. The window is
	 *  generous because an agent thinking, or waiting on a long analyze_asset,
	 *  is still there. */
	const CONNECTED_WITHIN = 120;
	const connected = $derived(lastSeen !== null && lastSeen <= CONNECTED_WITHIN);
	const line = $derived(agentLine({ serverError, connected, lastSeen, working }));
	const composerHint = $derived(
		connected ? 'Queued tasks run when your agent picks them up.' : 'Queued tasks wait here until you connect an agent.'
	);

	async function copy(text: string, key: string) {
		try {
			await navigator.clipboard.writeText(text);
			copied = key;
			setTimeout(() => {
				if (copied === key) copied = null;
			}, 1400);
		} catch {
			toast.error("Couldn't copy — select the text and copy manually");
		}
	}

	// Most actionable first: the agent's current work and anything awaiting review.
	const RANK: Record<TaskStatus, number> = { ready: 0, working: 1, queued: 2, failed: 3, done: 4 };
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

	// The setup card is for a panel with nothing in it; once a proposal lands it is the proposal that matters,
	// and at 1280×720 the card pushed it off the screen. (Asked for again, it opens again.)
	let hadStaged = false;
	$effect(() => {
		const has = staged !== null;
		if (has && !hadStaged) showConnect = false;
		hadStaged = has;
	});

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

	async function onApply() {
		applying = true;
		try {
			await applyProposal();
		} finally {
			applying = false;
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
			toast.success(connected ? 'Task queued for your agent' : 'Task queued — waiting for an agent');
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

	/** Leave the done edit in the queue's history. It is added only now, after the edit has run, so a
	 *  connected agent never sees a task it could claim and redo; failing to record it is not worth an error. */
	async function record(preset: string) {
		try {
			const task = await agent.add(preset);
			if (task) await agent.resolve(task.id);
		} catch {
			/* the edit is on the timeline and in the history; the queue entry is a courtesy */
		}
	}

	async function runPreset(p: string) {
		if (!isLocalPreset(p)) {
			try {
				await agent.add(p);
				toast.info(connected ? `Queued “${p}” for your agent` : `Queued “${p}” — waiting for an agent`);
			} catch (e) {
				toast.error(e instanceof Error ? e.message : String(e));
			}
			return;
		}
		if (editor.assets.length === 0) {
			toast.error('Import media first');
			return;
		}
		// The one preset that writes over something the user may care about: captions
		// from transcripts replace a set already on the cut, an imported one included.
		if (p === 'Caption the cut' && !(await confirmReplaceCaptions())) return;
		try {
			if (p === 'Remove silences') {
				if (!target) throw new Error('Import media first');
				// Only the silent spans are read: not a transcript, not the beat.
				await ui.ensureAnalysis(target.id, QUICK_EDIT_STEPS.silences);
				const before = cutSignature(editor.timeline);
				await editor.removeSilence(target.id);
				const out = silenceOutcome(target, cutSignature(editor.timeline) !== before);
				toast[out.kind](out.text);
				if (out.kind === 'success') await record(p);
			} else if (p === 'Cut to the beat') {
				// The grid comes from the music, so analyze whatever is on the audio tracks.
				const music = [
					...new Set(
						editor.timeline.tracks.filter((t) => t.kind === 'audio').flatMap((t) => t.clips.map((c) => c.asset_id))
					)
				];
				if (music.length === 0) throw new Error('Put music on an audio track first');
				for (const id of music) await ui.ensureAnalysis(id, QUICK_EDIT_STEPS.beat);
				const before = cutSignature(editor.timeline);
				await editor.snapToBeats();
				// A grid that does not reach the cuts leaves them alone; say so
				// rather than claiming an alignment that never happened.
				if (cutSignature(editor.timeline) === before) toast.info('No cuts were near a beat');
				else {
					toast.success('Aligned the cuts to the beat');
					await record(p);
				}
			} else if (p === 'Caption the cut') {
				// Captions read from the clips' transcripts, so analyze whatever is
				// in the cut but has not been transcribed yet.
				const sources = [...new Set(editor.timeline.tracks.flatMap((t) => t.clips.map((c) => c.asset_id)))];
				if (sources.length === 0) throw new Error('Put a clip on the timeline first');
				for (const id of sources) await ui.ensureAnalysis(id, QUICK_EDIT_STEPS.captions);
				await editor.generateCaptions();
				const n = (editor.timeline.overlays ?? []).filter((o) => o.generated).length;
				toast.success(`Captioned the cut — ${n} line${n === 1 ? '' : 's'}`, {
					action: { label: 'Undo', onClick: () => void editor.undo() }
				});
				await record(p);
			} else if (p === 'Frame for the delivery') {
				// Smart crop only matters once the project has a frame to be cut
				// for; without one the frame follows the footage and every shot
				// already fills it.
				await editor.smartCrop();
				toast.success('Framed every shot for the delivery frame', {
					action: { label: 'Undo', onClick: () => void editor.undo() }
				});
				await record(p);
			}
		} catch (e) {
			toast.error(e instanceof Error ? e.message : String(e));
		}
	}

	function iconColor(status: TaskStatus): string {
		if (status === 'working' || status === 'ready') return 'var(--agent-300)';
		if (status === 'done') return 'var(--green-400)';
		if (status === 'failed') return 'var(--red-400)';
		return 'var(--text-muted)';
	}

	/** The status line's button: open the setup card under it, or fold it away again. */
	function toggleConnect() {
		showConnect = !showConnect;
		if (showConnect) queueMicrotask(() => connectEl?.scrollIntoView({ block: 'nearest' }));
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
				(p): MenuItem => ({ label: `${isLocalPreset(p) ? 'Run now' : 'Queue for agent'}: ${p}`, icon: 'list-plus', disabled, action: () => void runPreset(p) })
			)
		];
		contextMenu.show(e, items);
	}
</script>

{#snippet secHead(label: string, right: string | null)}
	<div style="display:flex;align-items:center;gap:8px;margin-bottom:9px">
		<span class="k-overline">{label}</span>
		<div style="flex:1;height:var(--line-width);background:var(--border-subtle)"></div>
		{#if right}<span style="font-size:var(--text-sm);color:var(--text-muted)">{right}</span>{/if}
	</div>
{/snippet}

{#snippet copyRow(value: string, key: string)}
	<div
		style="display:flex;align-items:flex-start;gap:6px;background:var(--surface-inset);border:var(--line-width) solid var(--border-subtle);border-radius:var(--radius-sm);padding:6px 6px 6px 9px"
	>
		<code
			data-selectable
			style="flex:1;min-width:0;padding-top:4px;font-family:var(--font-mono);font-size:var(--text-sm);line-height:1.45;color:var(--text-secondary);white-space:pre-wrap;overflow-wrap:anywhere"
			>{value}</code
		>
		<IconBtn
			title={copied === key ? 'Copied' : 'Copy to clipboard'}
			size={28}
			onclick={() => copy(value, key)}
			style="color:{copied === key ? 'var(--green-400)' : ''}"
		>
			<Icon n={copied === key ? 'check' : 'copy'} s={13} />
		</IconBtn>
	</div>
{/snippet}

{#snippet taskCard(t: Task)}
	{@const s = STATUS_MAP[t.status]}
	{@const meta = metaFor(t)}
	<div
		style="border-radius:var(--radius-md);background:var(--surface-raised);border:var(--line-width) solid var(--border-default);border-left:{t.status ===
		'ready'
			? 'calc(var(--line-width) * 2) solid var(--agent-500)'
			: 'var(--line-width) solid var(--border-default)'};padding:10px 11px"
	>
		<div style="display:flex;align-items:center;gap:8px">
			<Icon n={s.icon} s={13} color={iconColor(t.status)} />
			<span
				style="flex:1;min-width:0;font-size:13px;font-weight:500;color:var(--text-primary);line-height:1.45;overflow-wrap:anywhere"
				use:tip={t.prompt}>{t.prompt}</span
			>
			{#if t.status !== 'ready'}
				<IconBtn title="Remove from queue" size={24} data-tone="quiet" onclick={() => dismissTask(t.id)}>
					<Icon n="x" s={13} />
				</IconBtn>
			{/if}
		</div>
		<div style="margin:7px 0 0 21px">
			<Badge tone={s.tone as 'neutral' | 'agent' | 'success' | 'danger'} dot={t.status === 'working'}>{s.label}</Badge>
		</div>
		{#if meta}
			<div data-selectable style="font-size:var(--text-sm);color:var(--text-muted);margin-top:7px;padding-left:21px">
				{t.status === 'queued' ? (connected ? 'Waiting for your agent to start' : 'Waiting for an agent') : meta}
			</div>
		{/if}
		{#if t.status === 'working'}
			<div style="margin-top:9px;padding-left:21px">
				<div
					style="position:relative;height:5px;border-radius:999px;background:var(--surface-inset);overflow:hidden;border:var(--line-width) solid var(--border-subtle)"
				>
					<div
						class="kerf-sweep"
						style="position:absolute;top:0;bottom:0;width:30%;background:linear-gradient(90deg, transparent, var(--agent-400), transparent)"
					></div>
				</div>
			</div>
		{/if}
		{#if t.status === 'ready'}
			{#if staged && readyCard(t, staged) === 'reviewing'}
				<div style="font-size:12px;color:var(--agent-300);margin-top:8px;padding-left:21px">
					{staged.diff.entries.length} proposed change{staged.diff.entries.length === 1 ? '' : 's'} in the review above — apply or discard them there
				</div>
			{:else}
				<div style="display:flex;gap:7px;margin-top:11px;padding-left:21px">
					<Btn variant="primary" size="sm" style="flex:1" onclick={() => resolveReady(t.id)}>Apply</Btn>
					<Btn size="sm" style="flex:1" onclick={() => dismissTask(t.id)}>Dismiss</Btn>
				</div>
			{/if}
		{/if}
	</div>
{/snippet}

<div
	role="presentation"
	bind:clientHeight={panelHeight}
	oncontextmenu={onAgentContextMenu}
	style="flex:1;min-height:0;background:var(--surface-panel);display:flex;flex-direction:column;overflow:hidden"
>
	<!-- One status line, not an `idle` badge over a "No agent connected" row. -->
	<div
		style="height:36px;flex:none;display:flex;align-items:center;gap:8px;padding:0 8px 0 12px;border-bottom:var(--line-width) solid var(--border-default)"
	>
		{#if line.working}
			<span class="kerf-spin" style="display:inline-flex;color:var(--agent-300)"><Icon n="loader" s={14} /></span>
		{:else}
			<Icon n={line.tone === 'danger' ? 'circle-alert' : 'plug'} s={14} color={line.tone === 'agent' ? 'var(--agent-300)' : line.tone === 'danger' ? 'var(--red-400)' : 'var(--text-muted)'} />
		{/if}
		<span
			role={line.tone === 'danger' ? 'alert' : 'status'}
			style="flex:1;min-width:0;font-size:12px;color:{line.tone === 'danger' ? 'var(--text-primary)' : 'var(--text-secondary)'};white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
			use:tip={line.text}>{line.text}</span
		>
		{#if !serverError}
			<Btn
				variant="ghost"
				size="sm"
				aria-expanded={showConnect}
				aria-controls="agent-connect"
				title="How to point an agent at this app"
				onclick={toggleConnect}>{connected ? 'Connection' : 'Connect'}</Btn
			>
		{/if}
	</div>

	<div style="flex:1;min-height:0;overflow-y:auto;padding:12px;display:flex;flex-direction:column;gap:12px">
		<!-- how to connect an agent: under the status line it answers -->
		{#if showConnect && !serverError}
			<div
				id="agent-connect"
				bind:this={connectEl}
				style="flex:none;border-radius:var(--radius-md);background:var(--surface-raised);border:var(--line-width) solid var(--border-default);padding:10px 11px;display:flex;flex-direction:column;gap:11px"
			>
				<div>
					<div style="font-size:12px;color:var(--text-muted);line-height:1.5;margin-bottom:6px">
						Point any MCP client at this local endpoint:
					</div>
					{@render copyRow(endpoint, 'endpoint')}
				</div>
				<div>
					<div style="font-size:12px;color:var(--text-muted);line-height:1.5;margin-bottom:6px">
						Using <span style="color:var(--text-secondary)">Claude Code</span>? Run this in your terminal:
					</div>
					{@render copyRow(claudeCmd, 'cmd')}
				</div>
				<div style="font-size:12px;color:var(--text-muted);line-height:1.5">
					The agent edits the project you have open and proposes cuts you review here. Override the address
					with <span style="font-family:var(--font-mono)">KERF_MCP_ADDR</span>.
				</div>
			</div>
		{/if}

		<!-- the agent's pending proposal -->
		{#if staged}
			<div
				style="flex:none;border-radius:var(--radius-md);background:var(--surface-raised);border:var(--line-width) solid var(--agent-border);border-left:calc(var(--line-width) * 2) solid var(--agent-500);overflow:clip"
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
					<div style="font-family:var(--font-mono);font-size:12px;color:var(--text-muted)">{headline}</div>
					<div style="font-size:12px;color:var(--text-muted);margin-top:3px">Nothing changes until you apply.</div>
				</div>

				{#if staged.stale}
					<div
						style="margin:9px 11px 0;padding:7px 9px;border-radius:var(--radius-sm);background:var(--diff-remove-surface);border:var(--line-width) solid var(--diff-remove);font-size:12px;color:var(--text-secondary);line-height:1.45"
					>
						You have edited the timeline since these were staged — applying replaces your newer cut.
					</div>
				{/if}

				<div style="padding:10px 11px 10px 32px;display:flex;flex-direction:column;gap:9px">
					{#each groups as group (group.label)}
						<div>
							<div class="k-overline" style="margin-bottom:4px">{group.label}</div>
							<div style="display:flex;flex-direction:column;gap:2px">
								{#each group.entries as e, i (`${e.kind}-${i}-${e.summary}`)}
									{@const tone = polarity(e.kind)}
									<button
										onclick={() => void inspectChange(e)}
										disabled={e.at == null && !e.clip_id && !e.overlay_id}
										use:tip={e.at == null && !e.clip_id && !e.overlay_id ? e.summary : `Look at this change: ${e.summary}`}
										style="display:flex;gap:7px;align-items:baseline;width:100%;text-align:left;padding:3px 4px;border:none;border-radius:var(--radius-sm);background:transparent;color:inherit;cursor:{e.at ==
										null && !e.clip_id && !e.overlay_id
											? 'default'
											: 'pointer'}"
									>
										<span
											style="flex:none;width:9px;font-family:var(--font-mono);font-size:12px;color:{polarityTint[tone]}"
											>{polarityMark[tone]}</span
										>
										<span style="flex:1;min-width:0;font-size:12px;line-height:1.45;color:var(--text-secondary)">
											{e.summary}
											{#if e.detail}<span style="color:var(--text-muted)"> · {e.detail}</span>{/if}
										</span>
									</button>
								{/each}
							</div>
						</div>
					{/each}
				</div>

			</div>
		{/if}

		<!-- queue -->
		<div style="flex:none">
			{@render secHead('Queue', agent.summary)}
			<div style="display:flex;flex-direction:column;gap:8px">
				{#if queue.length === 0}
					<div
						style="display:flex;flex-direction:column;align-items:center;gap:7px;padding:22px 16px;border-radius:var(--radius-md);border:var(--line-width) dashed var(--border-strong);background:var(--surface-inset);text-align:center"
					>
						<Icon n="list-plus" s={20} color="var(--text-muted)" />
						<div style="font-size:12px;color:var(--text-secondary)">No tasks queued</div>
						<div style="font-size:12px;color:var(--text-muted);line-height:1.5">
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

		<!-- One-click edits. Open by default: for most cuts these chips are the
		     whole reason to open the panel, and an agent is optional. -->
		<div style="flex:none">
			<button
				aria-expanded={quickOpen}
				aria-controls="agent-quick-edits"
				onclick={() => (quickOpen = !quickOpen)}
				style="display:flex;align-items:center;gap:8px;width:100%;margin-bottom:9px;padding:0;background:transparent;border:none;cursor:pointer;text-align:left"
			>
				<span class="k-overline">Quick edits</span>
				<div style="flex:1;height:var(--line-width);background:var(--border-subtle)"></div>
				<Icon n={quickOpen ? 'chevron-up' : 'chevron-down'} s={14} color="var(--text-muted)" />
			</button>
			{#if quickOpen}
				<div id="agent-quick-edits" style="display:flex;flex-direction:column;gap:8px">
					<div style="font-size:12px;color:var(--text-muted);line-height:1.45">
						<Icon n="play" s={11} style="vertical-align:-1px" /> runs now and lands in your history ·
						<Icon n="list-plus" s={11} style="vertical-align:-1px" /> is queued for your agent, which you review before it lands
					</div>
					<div style="display:flex;flex-wrap:wrap;gap:6px">
						{#each [true, false] as local (local)}
							{#each PRESETS.filter((p) => isLocalPreset(p) === local) as p (p)}
								{@const held = local && editor.previewingStaged}
								<Btn
									size="sm"
									disabled={disabled || held}
									icon={local ? 'play' : 'list-plus'}
									iconSize={12}
									title={held
										? 'Exit the preview first — this edits your cut'
										: { text: `${local ? 'Run now' : 'Queue for agent'}: ${p}`, detail: targetNote(p, target) }}
									onclick={() => runPreset(p)}>{p}</Btn
								>
							{/each}
						{/each}
					</div>
					{#if target}
						<div data-quick-target style="font-size:12px;color:var(--text-muted);line-height:1.45">
							Remove silences acts on <span style="color:var(--text-secondary)">{target.name}</span> — the footage of the selected clip, else the selected media.
						</div>
					{/if}
				</div>
			{/if}
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
							aria-expanded={openRevision === rev.seq}
							use:tip={'What this edit changed'}
							style="flex:1;min-width:0;text-align:left;background:none;border:none;padding:0;cursor:pointer"
						>
							<div
								style="font-size:12px;line-height:1.35;color:var(--text-secondary);white-space:nowrap;overflow:hidden;text-overflow:ellipsis"
							>
								{rev.label}
							</div>
							<div style="font-size:var(--text-xs);color:var(--text-muted)">
								{sourceLabel[rev.source]}
							</div>
						</button>
						{#if rev.current}
							<Badge tone="success" dot>now</Badge>
						{:else}
							<Btn variant="ghost" size="sm" icon="rotate-ccw" iconSize={12} title="Revert the timeline to this point" onclick={() => editor.revertTo(rev.seq)}>Revert</Btn>
						{/if}
					</div>
					{#if openRevision === rev.seq}
						{@const d = revisionDiffs[rev.seq]}
						<div style="padding:2px 4px 8px 26px;display:flex;flex-direction:column;gap:2px">
							{#if d === undefined}
								<span style="font-size:12px;color:var(--text-muted)">Reading the change…</span>
							{:else if d === null}
								<span style="font-size:12px;color:var(--text-muted)"
									>Change details are available in the desktop app.</span
								>
							{:else if d.entries.length === 0}
								<span style="font-size:12px;color:var(--text-muted)">Changed nothing.</span>
							{:else}
								<span style="font-family:var(--font-mono);font-size:var(--text-xs);color:var(--text-muted)"
									>{diffHeadline(d)}</span
								>
								{#each d.entries as e, i (`${rev.seq}-${i}`)}
									<span style="display:flex;gap:6px;align-items:baseline">
										<span
											style="flex:none;width:9px;font-family:var(--font-mono);font-size:12px;color:{polarityTint[
												polarity(e.kind)
											]}">{polarityMark[polarity(e.kind)]}</span
										>
										<span style="flex:1;min-width:0;font-size:12px;line-height:1.45;color:var(--text-secondary)"
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

	<!-- The review actions are the panel's, not the card's: pinned under the scrolling list, so Apply is on
	     screen whatever the list holds and whichever of the sections above is open (at 1280×720 the setup
	     card and a long list pushed it below the fold). -->
	{#if staged}
		<div
			data-review-actions
			style="flex:none;display:flex;align-items:center;gap:7px;padding:8px 12px;background:var(--surface-raised);border-top:var(--line-width) solid var(--agent-border)"
		>
			<Btn variant="primary" size="sm" style="flex:1" disabled={applying} onclick={onApply}>{applying ? 'Applying…' : 'Apply'}</Btn>
			<Btn
				size="sm"
				icon={editor.previewingStaged ? 'eye-off' : 'eye'}
				iconSize={13}
				aria-pressed={editor.previewingStaged}
				title={editor.previewingStaged ? 'Back to your cut' : 'Show the proposed cut in the editor'}
				onclick={togglePreview}
				style={editor.previewingStaged ? 'border-color:var(--agent-500);color:var(--agent-300)' : ''}
			>
				{editor.previewingStaged ? 'Exit preview' : 'Preview'}
			</Btn>
			<Btn size="sm" onclick={() => void discardProposal()}>Discard</Btn>
		</div>
	{/if}

	<!-- add task -->
	<div
		style="flex:none;padding:8px 12px;border-top:var(--line-width) solid var(--border-default);background:var(--surface-app);display:flex;flex-direction:column;gap:6px"
	>
		<div
			class="k-field"
			use:tip={composerHint}
			style="display:flex;align-items:center;gap:8px;height:32px;padding:0 4px 0 10px;background:var(--surface-inset);border:var(--line-width) solid var(--input);border-radius:var(--radius-sm);opacity:{disabled
				? 0.5
				: 1}"
		>
			<Icon n="list-plus" s={14} color="var(--text-muted)" />
			<input
				{disabled}
				bind:value={draft}
				onkeydown={onInputKey}
				aria-label="Task for your agent"
				aria-describedby="agent-composer-hint"
				placeholder="Describe a task for your agent…"
				style="flex:1;min-width:0;background:none;border:none;outline:none;color:var(--text-primary);font-family:var(--font-sans);font-size:13px"
			/>
			<IconBtn
				title="Queue task for agent"
				size={24}
				disabled={disabled || !draft.trim()}
				onclick={submit}
				style="color:{draft.trim() ? 'var(--kerf-300)' : ''}"
			>
				<Icon n="corner-down-left" s={14} />
			</IconBtn>
		</div>
		<span
			id="agent-composer-hint"
			class={showComposerHint(panelHeight) ? '' : 'sr-only'}
			style={showComposerHint(panelHeight) ? 'font-size:12px;color:var(--text-muted);line-height:1.4' : undefined}
		>
			{composerHint}
		</span>
	</div>
</div>
