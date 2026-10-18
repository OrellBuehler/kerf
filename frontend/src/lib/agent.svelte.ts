// Agent task queue state (Svelte 5 runes).
//
// The agent panel is an MCP task queue: the user enqueues tasks here, and a
// connected LLM claims and works them over MCP (the desktop app hosts the MCP
// server itself; see crates/kerf-app/src/mcp.rs). This singleton
// holds the queue the backend owns and the actions that mutate it; the panel
// renders `agent.tasks` directly.

import { addTask, listTasks, removeTask, resolveTask } from './api';
import { editor } from './state.svelte';
import { TASK_PAGE, keepNewest } from './paging';
import type { Task, TaskStatus } from './types';

class AgentQueue {
	tasks = $state<Task[]>([]);
	error = $state<string | null>(null);
	/** How many of the newest tasks are held; more are asked for (`showOlder`). Done tasks pile up for as long as the
	 *  project lives, and the queue is re-read on every agent edit. */
	limit = TASK_PAGE;
	/** There may be tasks older than the ones held. */
	more = $state(false);

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
