import { afterAll, afterEach, beforeEach, describe, expect, mock, test } from 'bun:test';
import './test-runes';
import type { Revision, StagedEdit, Timeline } from './types';

// The answers to an agent's proposal as every place that asks them runs them (`proposal-actions.ts`), over the one set
// of ticks the review card and the bar under the timeline share (`agent.svelte.ts`). The backend is faked; what is
// under test is that Apply means the same wherever it is pressed, that the way back is offered, and that a stale cut is
// asked about with the number of edits it holds. The singletons are put back as they were after every test.

const real = { ...(await import('./api')) };
const realNotifications = { ...(await import('./notifications.svelte')) };

const live: Timeline = { tracks: [{ id: 'v1', kind: 'video', name: 'V1', clips: [{ id: 'c1', asset_id: 'a', source_in: 0, source_out: 10, timeline_start: 0, volume: 1, fade_in: 0, fade_out: 0 }] }] };

const toasts: { kind: string; text: string; action?: { label: string; onClick: () => void }; duration?: number }[] = [];
const note =
	(kind: string) =>
	(text: string, opts?: { action?: { label: string; onClick: () => void }; duration?: number }) =>
		void toasts.push({ kind, text, action: opts?.action, duration: opts?.duration });
mock.module('./notifications.svelte', () => ({
	...realNotifications,
	toast: Object.assign(note('note'), { success: note('success'), warning: note('warning'), error: note('error'), info: note('info'), dismiss: () => {} })
}));

const entry = (id: string, at = 0) => ({ kind: 'clip_retrimmed' as const, summary: `change ${id}`, clip_id: 'c1', at, id });
const staged = (over: Partial<StagedEdit> = {}): StagedEdit => ({
	base_seq: 2,
	task_id: null,
	note: null,
	edits: ['Trim clip'],
	created_at: '2026-11-22T10:00:00Z',
	updated_at: '2026-11-22T10:00:00Z',
	stale: false,
	diff: { entries: [entry('e1'), entry('e2'), entry('e3')], duration_before: 10, duration_after: 6, clips_before: 1, clips_after: 1 },
	...over
});
const revs = (head: number, count = 8): Revision[] =>
	Array.from({ length: count }, (_, i) => ({ seq: i, label: `Edit ${i}`, source: 'user' as const, created_at: '2026-11-22T10:00:00Z', current: i === head }));

const calls: string[] = [];
const asked: string[] = [];
let answer = true;
let stagedNow: StagedEdit | null = null;
let historyNow: Revision[] = [];
let status = { endpoint: 'http://127.0.0.1:7777/mcp', last_seen_secs: null as number | null, error: null as string | null };
let statusReads = 0;

const backend = () => ({
	...real,
	getTimeline: async () => structuredClone(live),
	getStagedEdit: async () => stagedNow && structuredClone(stagedNow),
	getStagedTimeline: async () => structuredClone(live),
	discardedStagedEdit: async () => null,
	getHistory: async () => structuredClone(historyNow),
	applyStagedEdit: async (force: boolean) => {
		calls.push(`apply:${force}`);
		stagedNow = null;
		return structuredClone(live);
	},
	applyStagedSubset: async (ids: string[], force: boolean) => {
		calls.push(`subset:${[...ids].sort().join(',')}:${force}`);
		return { timeline: structuredClone(live), applied: ids.length, auto_included: [], skipped: [], closed: false, message: `Applied ${ids.length}` };
	},
	revertTo: async (seq: number) => {
		calls.push(`revert:${seq}`);
		historyNow = historyNow.map((r) => ({ ...r, current: r.seq === seq }));
		return structuredClone(live);
	},
	confirmAction: async (message: string) => {
		asked.push(message);
		return answer;
	},
	agentStatus: async () => {
		statusReads++;
		return { ...status };
	},
	mcpConnectionInfo: async () => real.mcpConnectionInfo(),
	onExportProgress: async () => () => {}
});
mock.module('./api', () => backend());
afterAll(() => {
	mock.module('./api', () => real);
	mock.module('./notifications.svelte', () => realNotifications);
});

const { editor } = await import('./state.svelte');
const { agent } = await import('./agent.svelte');
const actions = await import('./proposal-actions');

beforeEach(async () => {
	calls.length = 0;
	asked.length = 0;
	toasts.length = 0;
	answer = true;
	historyNow = revs(2);
	stagedNow = staged();
	status = { endpoint: 'http://127.0.0.1:7777/mcp', last_seen_secs: null, error: null };
	statusReads = 0;
	await editor.load();
});

afterEach(() => {
	stagedNow = null;
	editor.staged = null;
	editor.discarded = null;
	editor.previewingStaged = false;
	editor.history = [];
	agent.clearPicks();
	agent.proposedCut = null;
	agent.connectOpen = false;
	agent.lastSeen = null;
	agent.serverError = null;
});

describe('Apply is one answer wherever it is pressed', () => {
	test('with nothing ticked it is the whole proposal, and says how to take it back', async () => {
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['apply:false']);
		const done = toasts.find((t) => t.kind === 'success')!;
		expect(done.text).toBe('Applied the proposed changes');
		expect(done.action?.label).toBe('Undo');
		// long enough to be reached for: it undoes minutes of an agent's work
		expect(done.duration).toBeGreaterThan(4000);
	});

	test('ticked changes go alone, and the ticks are cleared once they are in', async () => {
		agent.togglePick('e1');
		agent.togglePick('e3');
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['subset:e1,e3:false']);
		expect([...agent.picks]).toEqual([]);
	});

	test('a tick made in the card is what the bar under the timeline applies: they read the same state', () => {
		agent.togglePick('e2');
		expect([...agent.picks]).toEqual(['e2']);
		expect(agent.applyText(false)).toBe('Apply 1 of 3');
		expect(agent.applyText(true)).toBe('Applying…');
		agent.clearPicks();
		expect(agent.applyText(false)).toBe('Apply');
	});

	test('a refusal is shown, and what was ticked stays ticked', async () => {
		mock.module('./api', () => ({ ...backend(), applyStagedSubset: async () => Promise.reject(new Error('that change no longer fits')) }));
		const { applyReview } = await import('./proposal-actions');
		agent.togglePick('e1');
		await editor.refreshStaged();
		expect(await applyReview()).toBe(false);
		expect(toasts.some((t) => t.kind === 'error' && t.text.includes('no longer fits'))).toBe(true);
		expect([...agent.picks]).toEqual(['e1']);
		mock.module('./api', () => backend());
	});
});

describe('a stale proposal is asked about with what it holds', () => {
	test('how many edits since, and that Undo is the way back; refused, nothing lands', async () => {
		stagedNow = staged({ stale: true, base_seq: 2 });
		historyNow = revs(5);
		await editor.refreshStaged();
		await editor.refreshHistory();
		answer = false;
		expect(await actions.applyReview()).toBe(false);
		expect(asked).toHaveLength(1);
		expect(asked[0]).toContain('You made 3 edits after these changes were staged');
		expect(asked[0]).toContain('Undo brings your cut back');
		expect(calls).toEqual([]);
	});

	test('accepted, it lands forced', async () => {
		stagedNow = staged({ stale: true });
		historyNow = revs(4);
		await editor.refreshStaged();
		await editor.refreshHistory();
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['apply:true']);
	});

	test('the count is the revision the cut is at less the one the proposal started from', async () => {
		stagedNow = staged({ base_seq: 2 });
		historyNow = revs(6);
		await editor.refreshStaged();
		await editor.refreshHistory();
		expect(actions.editsSinceStaged()).toBe(4);
		historyNow = revs(1);
		await editor.refreshHistory();
		expect(actions.editsSinceStaged()).toBe(-1);
		historyNow = [];
		await editor.refreshHistory();
		expect(actions.editsSinceStaged()).toBeNull();
	});
});

describe('the way out of a stale proposal that keeps the user’s work', () => {
	test('goes back to the revision the agent started from, and offers to undo that', async () => {
		stagedNow = staged({ stale: true, base_seq: 2 });
		historyNow = revs(5);
		await editor.refreshStaged();
		await editor.refreshHistory();
		expect(await actions.returnToProposalBase()).toBe(true);
		expect(calls).toEqual(['revert:2']);
		const said = toasts.find((t) => t.kind === 'info')!;
		expect(said.action?.label).toBe('Undo');
		said.action!.onClick();
		await Promise.resolve();
		expect(calls).toContain('revert:5');
	});

	test('is held while the proposal is previewed: the cut on screen is not the user’s', async () => {
		await editor.refreshStaged();
		editor.previewingStaged = true;
		expect(await actions.returnToProposalBase()).toBe(false);
		expect(calls).toEqual([]);
		// said as a notice with the way out, not as an error
		expect(toasts.map((t) => t.kind)).toEqual(['info']);
		expect(toasts[0].action?.label).toBe('Exit preview');
	});
});

describe('the ticks follow the proposal', () => {
	test('a change the proposal no longer has is not ticked any more', async () => {
		await editor.refreshStaged();
		agent.togglePick('e1');
		agent.togglePick('e2');
		stagedNow = staged({ diff: { entries: [entry('e2')], duration_before: 10, duration_after: 6, clips_before: 1, clips_after: 1 }, updated_at: '2026-11-22T10:05:00Z' });
		await editor.refreshStaged();
		agent.syncProposal();
		expect([...agent.picks]).toEqual(['e2']);
	});

	test('with no proposal there is nothing ticked', async () => {
		await editor.refreshStaged();
		agent.togglePick('e1');
		stagedNow = null;
		await editor.refreshStaged();
		agent.syncProposal();
		expect([...agent.picks]).toEqual([]);
	});
});

describe('who is connected is the singleton’s, shared by the panel and the first-run card', () => {
	test('watching asks at once, and who has spoken lately is connected', async () => {
		status = { ...status, last_seen_secs: 8 };
		const stop = agent.watch();
		await new Promise((r) => setTimeout(r, 5));
		expect(statusReads).toBe(1);
		expect(agent.lastSeen).toBe(8);
		expect(agent.connected).toBe(true);
		stop();
	});

	test('a second watcher shares the one timer and the first poll', async () => {
		const a = agent.watch();
		const b = agent.watch();
		await new Promise((r) => setTimeout(r, 5));
		expect(statusReads).toBe(1);
		a();
		a();
		b();
	});

	test('an agent not heard for more than two minutes is not connected, and a dead endpoint is kept', async () => {
		status = { ...status, last_seen_secs: 400, error: 'Port 7777 is already in use.' };
		const stop = agent.watch();
		await new Promise((r) => setTimeout(r, 5));
		expect(agent.connected).toBe(false);
		expect(agent.serverError).toBe('Port 7777 is already in use.');
		stop();
	});
});
