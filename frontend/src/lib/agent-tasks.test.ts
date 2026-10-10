import { afterAll, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';

const real = { ...(await import('./api')) };

const shown: { kind: string; text: string }[] = [];
const note = (kind: string) => (text: string) => void shown.push({ kind, text });
// `notifications.svelte` pulls in svelte-sonner's components, which bun cannot load.
mock.module('./notifications.svelte', () => ({
	toast: Object.assign(note('note'), {
		success: note('success'),
		info: note('info'),
		warning: note('warning'),
		error: note('error')
	}),
	notifications: {}
}));
let resolving: typeof real.resolveTask = real.resolveTask;
let removing: typeof real.removeTask = real.removeTask;
mock.module('./api', () => ({
	...real,
	resolveTask: (...a: Parameters<typeof real.resolveTask>) => resolving(...a),
	removeTask: (...a: Parameters<typeof real.removeTask>) => removing(...a)
}));

const { dismissTask, readyCard, resolveReady } = await import('./agent-tasks');
const { agent } = await import('./agent.svelte');

afterAll(() => {
	mock.module('./api', () => real);
});

beforeEach(() => {
	shown.length = 0;
	resolving = real.resolveTask;
	removing = real.removeTask;
});

describe('readyCard', () => {
	test('the task whose edits are staged is reviewed in the review card, so its own card has no Apply', () => {
		expect(readyCard({ id: 't2' }, { task_id: 't2' })).toBe('reviewing');
	});

	test('any other ready task is applied and dismissed from its own card', () => {
		expect(readyCard({ id: 't2' }, { task_id: 't9' })).toBe('resolve');
		expect(readyCard({ id: 't2' }, { task_id: null })).toBe('resolve');
		expect(readyCard({ id: 't2' }, { task_id: undefined })).toBe('resolve');
		expect(readyCard({ id: 't2' }, null)).toBe('resolve');
	});
});

describe('Apply and Dismiss on a queue card', () => {
	test('a refusal from the backend is an error toast, not an unhandled rejection', async () => {
		resolving = () => Promise.reject(new Error('the timeline changed since these edits were staged'));
		await resolveReady('t2');
		expect(shown).toEqual([{ kind: 'error', text: 'the timeline changed since these edits were staged' }]);
	});

	test('a bare string rejection (what Tauri sends) is shown too', async () => {
		removing = () => Promise.reject('task not found: t7');
		await dismissTask('t7');
		expect(shown).toEqual([{ kind: 'error', text: 'task not found: t7' }]);
	});

	test('on success it says nothing and the queue is the backend’s answer', async () => {
		await agent.load();
		await resolveReady('t3');
		expect(shown).toEqual([]);
		expect(agent.tasks.find((t) => t.id === 't3')?.status).toBe('done');
		await dismissTask('t3');
		expect(shown).toEqual([]);
		expect(agent.tasks.some((t) => t.id === 't3')).toBe(false);
	});
});
