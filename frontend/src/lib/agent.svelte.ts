// Agent task queue state (Svelte 5 runes).
//
// The agent panel is an MCP task queue: the user enqueues tasks here, and a
// connected LLM claims and works them over MCP (see kerf-mcp). This singleton
// holds the queue the backend owns and the actions that mutate it; the panel
// renders `agent.tasks` directly.

import { addTask, listTasks, removeTask, resolveTask } from './api';
import type { Task, TaskStatus } from './types';

class AgentQueue {
	tasks = $state<Task[]>([]);
	error = $state<string | null>(null);

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
			this.tasks = await listTasks();
			this.error = null;
		} catch (e) {
			this.error = this.#msg(e);
		}
	}

	async add(prompt: string): Promise<Task | null> {
		const trimmed = prompt.trim();
		if (!trimmed) return null;
		const task = await addTask(trimmed);
		this.tasks = [...this.tasks, task];
		return task;
	}

	async resolve(taskId: string): Promise<void> {
		this.tasks = await resolveTask(taskId);
	}

	async remove(taskId: string): Promise<void> {
		this.tasks = await removeTask(taskId);
	}

	#msg(e: unknown): string {
		return e instanceof Error ? e.message : String(e);
	}
}

export const agent = new AgentQueue();
