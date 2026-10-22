import { describe, expect, test } from 'bun:test';
import { agentTabLabel, ago, agentLine, COMPOSER_HINT_MIN_HEIGHT, proposalIndicator, queueEmptyHint, queuedChipLegend, REVIEW_CHIP_COUNT_WIDTH, REVIEW_CHIP_FULL_WIDTH, reviewChipText, reviewOffNote, showComposerHint } from './agent-status';
import { CONFIRM } from './confirm-copy';
import { STATUS_MAP } from './components/editor/data';
import type { StagedEdit, TimelineDiff } from './types';

const staged = (entries: number, stale = false): Pick<StagedEdit, 'diff' | 'stale'> => ({
	stale,
	diff: { entries: Array.from({ length: entries }, () => ({})), duration_before: 0, duration_after: 0 } as unknown as TimelineDiff
});

describe('how long ago', () => {
	test('seconds, minutes, hours — never zero seconds', () => {
		expect(ago(0)).toBe('1s ago');
		expect(ago(42)).toBe('42s ago');
		expect(ago(150)).toBe('3m ago');
		expect(ago(7300)).toBe('2h ago');
	});
});

describe('the agent status line', () => {
	const base = { serverError: null, connected: false, lastSeen: null, working: false };

	test('is one line for every state', () => {
		expect(agentLine(base)).toEqual({ text: 'No agent connected', tone: 'muted', working: false });
		expect(agentLine({ ...base, connected: true, lastSeen: 5 })).toEqual({ text: 'Agent connected', tone: 'agent', working: false });
		expect(agentLine({ ...base, lastSeen: 400 })).toEqual({ text: 'Last seen 7m ago', tone: 'muted', working: false });
		expect(agentLine({ ...base, connected: true, working: true })).toEqual({ text: 'Agent working', tone: 'agent', working: true });
	});

	test('a dead endpoint outranks everything, in the danger tone', () => {
		const line = agentLine({ ...base, connected: true, working: true, serverError: 'port 7777 is taken' });
		expect(line).toEqual({ text: 'port 7777 is taken', tone: 'danger', working: false });
	});
});

describe('the proposal indicator', () => {
	test('there is nothing to show without a staged edit', () => {
		expect(proposalIndicator(null)).toBeNull();
		expect(proposalIndicator(undefined)).toBeNull();
	});

	test('counts the changes and says so in the chip and the tooltip', () => {
		expect(proposalIndicator(staged(4))).toMatchObject({ count: 4, label: 'Review 4 changes', stale: false });
		expect(proposalIndicator(staged(4))!.tip).toBe('The agent proposes 4 changes — open the Agent panel to review them');
	});

	test('one change is singular', () => {
		expect(proposalIndicator(staged(1))).toMatchObject({ count: 1, label: 'Review 1 change' });
		expect(proposalIndicator(staged(1))!.tip).toContain('1 change —');
	});

	test('a proposal with no entries is still one waiting', () => {
		expect(proposalIndicator(staged(0))).toMatchObject({ count: 0, label: 'Review proposal' });
	});

	test('carries the stale flag', () => {
		expect(proposalIndicator(staged(2, true))!.stale).toBe(true);
	});
});

describe('the composer hint', () => {
	test('shows only when the panel is tall enough to spare the line', () => {
		expect(showComposerHint(COMPOSER_HINT_MIN_HEIGHT - 1)).toBe(false);
		expect(showComposerHint(COMPOSER_HINT_MIN_HEIGHT)).toBe(true);
		// 1366×768 puts the Agent panel near 420 px; 1920×1080, near 600
		expect(showComposerHint(420)).toBe(false);
		expect(showComposerHint(600)).toBe(true);
		expect(showComposerHint(0)).toBe(false);
	});
});

describe('a task’s badge', () => {
	test('queued, applied and failed are not the same neutral badge', () => {
		expect(STATUS_MAP.queued.tone).toBe('neutral');
		expect(STATUS_MAP.queued.icon).toBe('clock');
		expect(STATUS_MAP.done.tone).toBe('success');
		expect(STATUS_MAP.failed.tone).toBe('danger');
		expect(STATUS_MAP.failed.icon).toBe('circle-alert');
		const tones = Object.values(STATUS_MAP).map((s) => s.tone);
		expect(new Set(tones).size).toBeGreaterThanOrEqual(4);
	});
});

describe('the title bar chip', () => {
	const four = proposalIndicator(staged(4))!;

	test('is the whole label while the cell has room, or is not measured yet', () => {
		expect(reviewChipText(four, 0)).toBe('Review 4 changes');
		expect(reviewChipText(four, REVIEW_CHIP_FULL_WIDTH)).toBe('Review 4 changes');
		expect(reviewChipText(four, 1920 / 2 - 150)).toBe('Review 4 changes');
	});

	test('1280×720 (a right cell of about 470) and 1366×768 (513) say what the number counts, not a bare 4', () => {
		expect(reviewChipText(four, 470)).toBe('4 changes');
		expect(reviewChipText(four, 513)).toBe('4 changes');
		expect(reviewChipText(proposalIndicator(staged(1))!, 470)).toBe('1 change');
	});

	test('says what the number counts when the cell has most of the room — 1440×900 (about 552) is one', () => {
		expect(reviewChipText(four, 552)).toBe('4 changes');
		expect(reviewChipText(four, REVIEW_CHIP_FULL_WIDTH - 1)).toBe('4 changes');
		expect(reviewChipText(four, REVIEW_CHIP_COUNT_WIDTH)).toBe('4 changes');
		expect(reviewChipText(proposalIndicator(staged(1))!, 552)).toBe('1 change');
	});

	test('is the count alone in a narrow cell, and a word when there is no count', () => {
		expect(reviewChipText(four, REVIEW_CHIP_COUNT_WIDTH - 1)).toBe('4');
		expect(reviewChipText(four, 300)).toBe('4');
		expect(reviewChipText(proposalIndicator(staged(0))!, 300)).toBe('Review');
		expect(reviewChipText(proposalIndicator(staged(0))!, 552)).toBe('Review');
	});
});

describe('the Agent tab’s name', () => {
	test('is the visible label, then what the count means', () => {
		expect(agentTabLabel(null)).toBe('Agent');
		expect(agentTabLabel(proposalIndicator(staged(4)))).toBe('Agent, 4 changes to review');
		expect(agentTabLabel(proposalIndicator(staged(1)))).toBe('Agent, 1 change to review');
		expect(agentTabLabel(proposalIndicator(staged(0)))).toBe('Agent, proposal to review');
	});

	test('starts with the word on the tab, so a voice command for the visible label still works', () => {
		expect(agentTabLabel(proposalIndicator(staged(2))).startsWith('Agent')).toBe(true);
	});
});

describe('what the panel says while review is off', () => {
	test('task work lands too, so the strip says so', () => {
		expect(reviewOffNote(false)).toContain('task work included');
		expect(reviewOffNote(false)).toContain('land on your timeline');
	});

	test('a proposal that is still open keeps taking the agent’s edits, and the strip does not claim they land', () => {
		const note = reviewOffNote(true);
		expect(note).toContain('proposal is still open');
		expect(note).toContain('apply or discard');
		expect(note).not.toContain('land on your timeline');
	});

	test('the empty queue and the queued chip say where the agent’s work goes', () => {
		expect(queueEmptyHint(true)).toContain('proposes edits');
		expect(queueEmptyHint(false)).toContain('edits your timeline directly');
		expect(queuedChipLegend(true)).toContain('you review before it lands');
		expect(queuedChipLegend(false)).toContain('land as it makes them');
	});

	test('a task the agent finished with review off reads as applied, not as waiting for review', () => {
		expect(STATUS_MAP.done.label).toBe('Applied');
		expect(STATUS_MAP.ready.label).toBe('Ready to review');
	});

	test('turning review off says task work lands too and that an open proposal stays', () => {
		const { message } = CONFIRM.agentReviewOff;
		expect(message).toContain('tasks you queue');
		expect(message).toContain('already waiting stays');
	});
});
