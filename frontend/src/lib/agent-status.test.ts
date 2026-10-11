import { describe, expect, test } from 'bun:test';
import { ago, agentLine, COMPOSER_HINT_MIN_HEIGHT, proposalIndicator, REVIEW_CHIP_FULL_WIDTH, reviewChipText, showComposerHint } from './agent-status';
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
		expect(proposalIndicator(staged(4))!.tip).toBe('The agent proposed 4 changes — open the Agent panel to apply or discard them');
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
		expect(reviewChipText(four, 1366 / 2 - 150)).toBe('Review 4 changes');
	});

	test('is the count alone in a narrow cell, and a word when there is no count', () => {
		expect(reviewChipText(four, REVIEW_CHIP_FULL_WIDTH - 1)).toBe('4');
		expect(reviewChipText(proposalIndicator(staged(0))!, 300)).toBe('Review');
	});
});
