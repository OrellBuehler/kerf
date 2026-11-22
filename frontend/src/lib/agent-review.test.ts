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
	Array.from({ length: count }, (_, i) => ({ seq: i, label: `Edit ${i}`, source: 'user' as const, created_at: `2026-11-22T10:00:${String(i % 60).padStart(2, '0')}Z`, current: i === head }));

const calls: string[] = [];
const asked: string[] = [];
let answer = true;
let stagedNow: StagedEdit | null = null;
let historyNow: Revision[] = [];
let revertFails: string | null = null;
/** What the backend says about the proposal after a revert: it is the backend's word, not the frontend's. */
let staleAfterRevert = false;
let status = { endpoint: 'http://127.0.0.1:7777/mcp', last_seen_secs: null as number | null, error: null as string | null };
let statusReads = 0;

const backend = () => ({
	...real,
	getTimeline: async () => structuredClone(live),
	getStagedEdit: async () => stagedNow && structuredClone(stagedNow),
	getStagedTimeline: async () => structuredClone(live),
	discardedStagedEdit: async () => null,
	getHistory: async (limit?: number) => structuredClone(limit ? historyNow.slice(-limit) : historyNow),
	undo: async () => {
		calls.push('undo');
		const at = historyNow.findIndex((r) => r.current);
		if (at > 0) historyNow = historyNow.map((r, i) => ({ ...r, current: i === at - 1 }));
		return structuredClone(live);
	},
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
		if (revertFails) throw new Error(revertFails);
		historyNow = historyNow.map((r) => ({ ...r, current: r.seq === seq }));
		if (stagedNow) stagedNow = { ...stagedNow, stale: staleAfterRevert };
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
	revertFails = null;
	staleAfterRevert = false;
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
	agent.syncProposal();
	agent.observeProposal(null);
	agent.proposedCut = null;
	agent.connectOpen = false;
	agent.lastSeen = null;
	agent.serverError = null;
});

/** Look at the proposal and the history the way the page does: the proposal fits, and is noted fitting. */
async function look(over: Partial<StagedEdit> = {}, head = 2) {
	stagedNow = staged(over);
	historyNow = revs(head);
	await editor.refreshStaged();
	await editor.refreshHistory();
	agent.observeProposal(editor.staged);
}
/** The user edits: the history moves on and the backend says the cut has changed. */
async function edit(toHead: number, count = toHead + 1) {
	historyNow = revs(toHead, count);
	stagedNow = { ...stagedNow!, stale: true };
	await editor.refreshHistory();
	await editor.refreshStaged();
}

describe('Apply is one answer wherever it is pressed', () => {
	test('with nothing ticked it is the whole proposal, and says how to take it back', async () => {
		await look();
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['apply:false']);
		const done = toasts.find((t) => t.kind === 'success')!;
		expect(done.text).toBe('Applied the proposed changes');
		expect(done.action?.label).toBe('Undo');
		// long enough to be reached for: it undoes minutes of an agent's work
		expect(done.duration).toBeGreaterThan(4000);
	});

	test('ticked changes go alone, and the ticks are cleared once they are in', async () => {
		await look();
		agent.togglePick('e1');
		agent.togglePick('e3');
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['subset:e1,e3:false']);
		expect([...agent.picks]).toEqual([]);
	});

	test('ticking every change is Apply: the same path and the same Undo, not a partial apply that merges onto the cut', async () => {
		await look();
		for (const id of ['e1', 'e2', 'e3']) agent.togglePick(id);
		expect(agent.coversAll).toBe(true);
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['apply:false']);
		expect(toasts.find((t) => t.kind === 'success')!.action?.label).toBe('Undo');
		expect([...agent.picks]).toEqual([]);
	});

	test('not every change ticked is not the whole proposal', async () => {
		await look();
		agent.togglePick('e1');
		expect(agent.coversAll).toBe(false);
		expect(agent.applyText(false)).toBe('Apply 1 of 3');
	});

	test('a tick made in the card is what the bar under the timeline applies: they read the same state', async () => {
		await look();
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
		await look();
		agent.togglePick('e1');
		expect(await applyReview()).toBe(false);
		expect(toasts.some((t) => t.kind === 'error' && t.text.includes('no longer fits'))).toBe(true);
		expect([...agent.picks]).toEqual(['e1']);
		mock.module('./api', () => backend());
	});
});

describe('a stale proposal is asked about with what it holds', () => {
	test('how many edits since, and that Undo is the way back; refused, nothing lands', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(5);
		answer = false;
		expect(await actions.applyReview()).toBe(false);
		expect(asked).toHaveLength(1);
		expect(asked[0]).toContain('You made 3 edits after these changes were staged');
		expect(asked[0]).toContain('in place of your cut');
		expect(asked[0]).toContain('Undo brings your cut back');
		expect(calls).toEqual([]);
	});

	test('a partial apply is told what a partial apply does, not that the cut is replaced', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(4);
		agent.togglePick('e1');
		await actions.applyReview();
		expect(asked).toHaveLength(1);
		expect(asked[0]).not.toContain('in place of your cut');
		expect(asked[0]).toContain('puts each onto your cut as it is now');
		expect(calls).toEqual(['subset:e1:true']);
	});

	test('accepted, it lands forced', async () => {
		await look({}, 2);
		await edit(4);
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['apply:true']);
	});

	test('the count is the revisions since the one the proposal was seen fitting at', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(6);
		expect(actions.editsSinceStaged()).toBe(4);
		historyNow = revs(1);
		await editor.refreshHistory();
		expect(actions.editsSinceStaged()).toBe(-1);
	});

	test('no count is given that cannot be stood behind: the number was recycled, or it was never seen fitting, or the head is unknown', async () => {
		await look({ base_seq: 2 }, 2);
		// an undo and a new edit: another revision has number 2 now (made at another time)
		historyNow = revs(3).map((r) => (r.seq === 2 ? { ...r, created_at: '2026-11-22T10:30:00Z' } : r));
		stagedNow = { ...stagedNow!, stale: true };
		await editor.refreshHistory();
		await editor.refreshStaged();
		expect(actions.editsSinceStaged()).toBeNull();
		// first seen stale: the revision it started from was never seen
		agent.observeProposal(null);
		agent.observeProposal(editor.staged);
		expect(editor.staged!.stale).toBe(true);
		expect(actions.editsSinceStaged()).toBeNull();
		// head outside the page read
		await look({ base_seq: 2 }, 2);
		historyNow = revs(-1, 6);
		await editor.refreshHistory();
		expect(actions.editsSinceStaged()).toBeNull();
	});

	test('the stale prompt then says the cut changed and gives no number', async () => {
		await look({ base_seq: 2 }, 2);
		historyNow = revs(3, 4).map((r) => (r.seq === 2 ? { ...r, created_at: '2026-11-22T10:30:00Z' } : r));
		stagedNow = { ...stagedNow!, stale: true };
		await editor.refreshHistory();
		await editor.refreshStaged();
		await actions.applyReview();
		expect(asked[0].startsWith('You edited the timeline after these changes were staged.')).toBe(true);
	});
});

describe('applying drops the edits the cut was stepped back from, so it asks', () => {
	test('go back to where the proposal started, then Apply: names the edit it drops and says it cannot come back', async () => {
		await look({ base_seq: 0 }, 0);
		historyNow = [...revs(0, 1), { seq: 1, label: 'Remove clip', source: 'user', created_at: '2026-11-22T10:00:09Z', current: false }];
		await editor.refreshHistory();
		answer = false;
		expect(await actions.applyReview()).toBe(false);
		expect(asked).toHaveLength(1);
		expect(asked[0]).toContain('Applying drops the 1 edit you stepped back from: “Remove clip”');
		expect(asked[0]).toContain('cannot be brought back');
		expect(asked[0]).toContain('go forward first');
		expect(calls).toEqual([]);
		answer = true;
		expect(await actions.applyReview()).toBe(true);
		expect(calls).toEqual(['apply:false']);
	});

	test('a partial apply asks the same, from the card or the bar: both are `applyReview`', async () => {
		await look({ base_seq: 0 }, 0);
		historyNow = [...revs(0, 1), { seq: 1, label: 'Remove clip', source: 'user', created_at: '2026-11-22T10:00:09Z', current: false }];
		await editor.refreshHistory();
		agent.togglePick('e1');
		answer = false;
		expect(await actions.applyReview()).toBe(false);
		expect(asked[0]).toContain('drops the 1 edit you stepped back from');
		expect(calls).toEqual([]);
	});

	test('nothing to ask when the cut is at its newest edit and fits', async () => {
		await look({}, 7);
		expect(await actions.applyReview()).toBe(true);
		expect(asked).toEqual([]);
	});

	test('stale and stepped back is one question', async () => {
		await look({ base_seq: 2 }, 2);
		historyNow = revs(2, 5);
		stagedNow = { ...stagedNow!, stale: true };
		await editor.refreshHistory();
		await editor.refreshStaged();
		await actions.applyReview();
		expect(asked).toHaveLength(1);
		expect(asked[0]).toContain('drops the 2 edits you stepped back from');
		expect(asked[0]).toContain('The cut has also changed');
	});

	test('a head older than the page read is found before the question is asked: the page is widened until it holds it', async () => {
		await look({ base_seq: 0 }, 0);
		// 250 edits, the cut at 5: the newest 100 hold no current row
		historyNow = revs(5, 250);
		await editor.refreshHistory();
		expect(editor.history.some((r) => r.current)).toBe(false);
		expect(editor.historyMore).toBe(true);
		answer = false;
		await actions.applyReview();
		expect(editor.history.some((r) => r.current)).toBe(true);
		expect(asked[0]).toContain('Applying drops the 244 edits you stepped back from');
		editor.historyLimit = 100;
	});

	test('go forward puts the cut at its newest edit, and says how to undo that', async () => {
		await look({}, 0);
		historyNow = revs(0, 4);
		await editor.refreshHistory();
		expect(await actions.goForwardToLatest()).toBe(true);
		expect(calls).toEqual(['revert:3']);
		const said = toasts.find((t) => t.kind === 'info')!;
		expect(said.action?.label).toBe('Undo');
		said.action!.onClick();
		await new Promise((r) => setTimeout(r, 5));
		expect(calls).toEqual(['revert:3', 'revert:0']);
	});
});

describe('an Undo on a toast acts only while the edit it names is the newest', () => {
	test('applied, nothing since: Undo undoes the apply', async () => {
		await look();
		await actions.applyReview();
		const undo = toasts.find((t) => t.kind === 'success')!.action!;
		undo.onClick();
		await new Promise((r) => setTimeout(r, 5));
		expect(calls).toEqual(['apply:false', 'undo']);
	});

	test('applied, then the user deleted a clip: Undo says so and does not undo the delete', async () => {
		await look();
		await actions.applyReview();
		const undo = toasts.find((t) => t.kind === 'success')!.action!;
		// the user's own edit lands after the apply
		historyNow = [...historyNow.map((r) => ({ ...r, current: false })), { seq: historyNow.length, label: 'Remove clip', source: 'user', created_at: '2026-11-22T10:20:00Z', current: true }];
		undo.onClick();
		await new Promise((r) => setTimeout(r, 5));
		expect(calls).toEqual(['apply:false']);
		expect(toasts.at(-1)!.kind).toBe('info');
		expect(toasts.at(-1)!.text).toContain('edited since');
	});

	test('the same revision number made later is not the same revision', async () => {
		await look();
		await actions.applyReview();
		const undo = toasts.find((t) => t.kind === 'success')!.action!;
		const head = historyNow.find((r) => r.current)!;
		historyNow = historyNow.map((r) => (r.seq === head.seq ? { ...r, created_at: '2026-11-22T10:59:00Z' } : r));
		undo.onClick();
		await new Promise((r) => setTimeout(r, 5));
		expect(calls).toEqual(['apply:false']);
		expect(toasts.at(-1)!.text).toContain('edited since');
	});

	test('with the head unknown there is no Undo to offer', async () => {
		await look();
		historyNow = revs(-1, 3);
		await editor.refreshHistory();
		const { undoAction } = await import('./undo-guard');
		expect(undoAction()).toBeUndefined();
	});
});

describe('the way out of a stale proposal', () => {
	test('goes back to the revision it was seen fitting at, and offers an Undo that holds only while that is the newest', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(5);
		expect(await actions.returnToProposalBase()).toBe(true);
		expect(calls).toEqual(['revert:2']);
		const said = toasts.find((t) => t.kind === 'info')!;
		expect(said.text).toBe('Went back to the cut the proposal started from');
		expect(said.action?.label).toBe('Undo');
		said.action!.onClick();
		await new Promise((r) => setTimeout(r, 5));
		expect(calls).toEqual(['revert:2', 'revert:5']);
	});

	test('its Undo does nothing once the cut was edited after going back', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(5);
		await actions.returnToProposalBase();
		const said = toasts.find((t) => t.kind === 'info')!;
		historyNow = [...historyNow.filter((r) => r.seq <= 2).map((r) => ({ ...r, current: false })), { seq: 3, label: 'Trim clip', source: 'user', created_at: '2026-11-22T10:40:00Z', current: true }];
		said.action!.onClick();
		await new Promise((r) => setTimeout(r, 5));
		expect(calls).toEqual(['revert:2']);
		expect(toasts.at(-1)!.text).toContain('edited since');
	});

	test('if the backend says it still does not fit, that is said and no success is claimed, with the apply that goes over the cut', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(5);
		staleAfterRevert = true;
		expect(await actions.returnToProposalBase()).toBe(true);
		expect(toasts.some((t) => t.text === 'Went back to the cut the proposal started from')).toBe(false);
		const said = toasts.find((t) => t.kind === 'warning')!;
		expect(said.text).toContain('still does not fit');
		expect(said.action?.label).toBe('Apply…');
	});

	test('a revision that is no longer there is a sentence, not `revision not found: 7`', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(5);
		revertFails = 'revision not found: 2';
		expect(await actions.returnToProposalBase()).toBe(false);
		const said = toasts.at(-1)!;
		expect(said.kind).toBe('error');
		expect(said.text).toContain('no longer in History');
		expect(said.text).not.toContain('revision not found');
		expect(actions.revertRefusal(new Error('boom'))).toBe('boom');
	});

	test('with the revision not in the history there is nothing to go back to, and it says so without asking the backend', async () => {
		await look({ base_seq: 2 }, 2);
		historyNow = revs(5).map((r) => (r.seq === 2 ? { ...r, created_at: '2026-11-22T10:30:00Z' } : r));
		stagedNow = { ...stagedNow!, stale: true };
		await editor.refreshHistory();
		await editor.refreshStaged();
		expect(await actions.returnToProposalBase()).toBe(false);
		expect(calls).toEqual([]);
		expect(toasts.at(-1)!.text).toContain('no longer in History');
	});

	test('is held while the proposal is previewed: the cut on screen is not the user’s', async () => {
		await look({ base_seq: 2 }, 2);
		await edit(5);
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
		await look();
		agent.syncProposal();
		agent.togglePick('e1');
		agent.togglePick('e2');
		stagedNow = staged({ diff: { entries: [entry('e2')], duration_before: 10, duration_after: 6, clips_before: 1, clips_after: 1 }, updated_at: '2026-11-22T10:05:00Z' });
		await editor.refreshStaged();
		agent.syncProposal();
		expect([...agent.picks]).toEqual(['e2']);
	});

	test('with no proposal there is nothing ticked', async () => {
		await look();
		agent.syncProposal();
		agent.togglePick('e1');
		stagedNow = null;
		await editor.refreshStaged();
		agent.syncProposal();
		expect([...agent.picks]).toEqual([]);
	});

	test('a different proposal with changes of the same ids starts with nothing ticked', async () => {
		await look();
		agent.syncProposal();
		agent.togglePick('e1');
		agent.togglePick('e2');
		stagedNow = staged({ created_at: '2026-11-22T11:30:00Z', updated_at: '2026-11-22T11:30:00Z' });
		await editor.refreshStaged();
		agent.syncProposal();
		expect([...agent.picks]).toEqual([]);
		expect(agent.proposedCut).toBeNull();
	});

	test('the same proposal keeps its ticks as the agent adds to it', async () => {
		await look();
		agent.syncProposal();
		agent.togglePick('e1');
		stagedNow = staged({ updated_at: '2026-11-22T10:05:00Z' });
		await editor.refreshStaged();
		agent.syncProposal();
		expect([...agent.picks]).toEqual(['e1']);
	});
});

describe('a proposal arriving is announced once', () => {
	test('the first sight of it is news; the same proposal again is not', () => {
		expect(agent.announce({ created_at: 'p1' })).toBe(true);
		expect(agent.announce({ created_at: 'p1' })).toBe(false);
		expect(agent.announce({ created_at: 'p2' })).toBe(true);
	});

	test('one the user brings back themselves is not an arrival', async () => {
		stagedNow = null;
		editor.staged = null;
		editor.discarded = staged({ created_at: 'p-restored' });
		mock.module('./api', () => ({ ...backend(), restoreDiscardedEdits: async () => structuredClone(staged({ created_at: 'p-restored' })) }));
		const { restoreProposal } = await import('./proposal-actions');
		await restoreProposal();
		expect(agent.announce({ created_at: 'p-restored' })).toBe(false);
		mock.module('./api', () => backend());
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
