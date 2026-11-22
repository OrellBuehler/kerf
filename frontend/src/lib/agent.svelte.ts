// Agent task queue state (Svelte 5 runes).
//
// The agent panel is an MCP task queue: the user enqueues tasks here, and a
// connected LLM claims and works them over MCP (the desktop app hosts the MCP
// server itself; see crates/kerf-app/src/mcp.rs). This singleton
// holds the queue the backend owns and the actions that mutate it; the panel
// renders `agent.tasks` directly.

import { addTask, agentStatus, getStagedTimeline, listTasks, mcpConnectionInfo, removeTask, resolveTask } from './api';
import { isConnected } from './agent-status';
import { editor } from './state.svelte';
import { TASK_PAGE, keepNewest } from './paging';
import { applyLabel, comingAlong, keepPresent, pickRows, toggled, toggledGroup } from './review-picks';
import type { McpConnectionInfo, Task, TaskStatus, Timeline } from './types';

/** How often the endpoint is asked who has spoken to it: an agent arriving is not an event the app hears. */
const STATUS_EVERY_MS = 5000;

class AgentQueue {
	tasks = $state<Task[]>([]);
	error = $state<string | null>(null);
	/** How many of the newest tasks are held; more are asked for (`showOlder`). Done tasks pile up for as long as the
	 *  project lives, and the queue is re-read on every agent edit. */
	limit = TASK_PAGE;
	/** There may be tasks older than the ones held. */
	more = $state(false);

	// ---- the connection ---------------------------------------------------------------------------------------------
	// Kept here, not in the Agent panel, so the first-run card and the panel say the same thing about whether an agent is
	// there. A streamable-HTTP client holds no connection between calls: "connected" is "has spoken lately".

	/** The setup card under the Agent panel's status line is open. The first-run card's button sets it, so the click lands
	 *  on the setup rather than on a panel that still needs a second click. */
	connectOpen = $state(false);
	/** Seconds since an agent last spoke to the endpoint (when it was last asked), or null if none ever has. */
	lastSeen = $state<number | null>(null);
	/** Why the endpoint is not listening (the port is taken), or null while it is. */
	serverError = $state<string | null>(null);
	endpoint = $state('http://127.0.0.1:7777/mcp');
	/** The connection as the backend describes it: the token, and the setup for each client. Null until it is read. */
	mcp = $state<McpConnectionInfo | null>(null);
	#watchers = 0;
	#timer: ReturnType<typeof setInterval> | null = null;

	get connected(): boolean {
		return isConnected(this.lastSeen);
	}

	async #poll() {
		try {
			const s = await agentStatus();
			this.endpoint = s.endpoint;
			this.lastSeen = s.last_seen_secs;
			this.serverError = s.error;
			this.mcp = await mcpConnectionInfo();
		} catch {
			/* the endpoint is part of the app; a failed read is not worth a toast */
		}
	}

	/** Ask the endpoint now and every few seconds until the returned function is called. Several watchers (the panel, the
	 *  first-run card) share one timer. */
	watch(): () => void {
		if (this.#watchers++ === 0) {
			void this.#poll();
			this.#timer = setInterval(() => void this.#poll(), STATUS_EVERY_MS);
		}
		let stopped = false;
		return () => {
			if (stopped) return;
			stopped = true;
			if (--this.#watchers === 0 && this.#timer) {
				clearInterval(this.#timer);
				this.#timer = null;
			}
		};
	}

	// ---- the proposal under review: which of its changes are ticked ---------------------------------------------------
	// Here, not in the card, because the Apply under the timeline and the one in the card are one answer: both apply what is ticked.

	picks = $state<ReadonlySet<string>>(new Set());
	/** The proposed cut, read while something is ticked: what the changes that come along with a pick are worked out against. */
	proposedCut = $state<Timeline | null>(null);
	#cutStamp: string | null = null;

	/** The changes of the open proposal, as the card lists them (the timeline's and the library's). */
	get rows() {
		const s = editor.staged;
		return s ? pickRows(s.diff.entries, s.asset_changes ?? []) : [];
	}

	/** The changes that would be applied without being ticked, and why (`comingAlong`). */
	get along(): Map<string, string> {
		const s = editor.staged;
		if (!s) return new Map();
		return comingAlong(s.diff.entries, this.picks, { live: editor.liveTimeline, proposed: this.proposedCut });
	}

	/** What Apply says: `Apply`, or `Apply 3 of 4` while some are ticked. */
	applyText(busy: boolean): string {
		return applyLabel(this.picks.size + this.along.size, this.rows.length, busy);
	}

	togglePick(id: string) {
		this.picks = toggled(this.picks, id);
	}
	togglePickGroup(ids: readonly string[]) {
		this.picks = toggledGroup(this.picks, ids);
	}
	clearPicks() {
		this.picks = new Set();
	}

	/** Keep the ticks to changes the proposal still has, and read the proposed cut for the ones that come along. Run from an
	 *  effect over the proposal; idempotent. */
	syncProposal() {
		const kept = keepPresent(this.picks, this.rows);
		if (kept !== this.picks) this.picks = kept;
		const stamp = editor.staged?.updated_at;
		if (!stamp || this.picks.size === 0 || (stamp === this.#cutStamp && this.proposedCut)) return;
		this.#cutStamp = stamp;
		void getStagedTimeline().then(
			(t) => {
				if (editor.staged?.updated_at === stamp) this.proposedCut = t;
			},
			() => {}
		);
	}

	/** True while an agent is actively working a task. */
	get working(): boolean {
		return this.tasks.some((t) => t.status === 'working');
	}

	countOf(status: TaskStatus): number {
		return this.tasks.filter((t) => t.status === status).length;
	}

	/** Compact queue summary for the section header, e.g. "1 working · 2 queued". */
	get summary(): string {
		const order: [TaskStatus, string][] = [
			['working', 'working'],
			['ready', 'ready'],
			['queued', 'queued'],
			['failed', 'failed'],
			['done', 'done']
		];
		const parts = order
			.map(([s, label]) => [this.countOf(s), label] as const)
			.filter(([n]) => n > 0)
			.map(([n, label]) => `${n} ${label}`);
		return parts.length ? parts.join(' · ') : 'no tasks';
	}

	async load() {
		try {
			this.set(await listTasks(this.limit));
			this.error = null;
		} catch (e) {
			this.error = this.#msg(e);
		}
	}

	/** Hold a queue the backend answered with: the newest `limit` of it, and whether more may be behind. */
	private set(tasks: Task[]) {
		this.tasks = keepNewest(tasks, this.limit);
		this.more = tasks.length >= this.limit;
	}

	/** Ask for another page of older tasks. */
	async showOlder() {
		this.limit += TASK_PAGE;
		await this.load();
	}

	async add(prompt: string): Promise<Task | null> {
		const trimmed = prompt.trim();
		if (!trimmed) return null;
		const task = await addTask(trimmed);
		this.tasks = [...this.tasks, task];
		return task;
	}

	/** Accept a task — and with it any edits staged under it, which the backend
	 *  applies in the same call, so the timeline and history have to re-read. */
	async resolve(taskId: string): Promise<void> {
		this.set(await resolveTask(taskId));
		await editor.refreshTimeline();
		await editor.refreshHistory();
	}

	/** Dismiss a task, discarding any proposal staged under it. */
	async remove(taskId: string): Promise<void> {
		this.set(await removeTask(taskId));
		await editor.refreshTimeline();
	}

	#msg(e: unknown): string {
		return e instanceof Error ? e.message : String(e);
	}
}

export const agent = new AgentQueue();
