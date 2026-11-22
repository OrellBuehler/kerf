import { describe, expect, test } from 'bun:test';
import { absoluteTime, backNote, branchSentence, goLabel, goName, goTip, historyRows, needsOlderForHead, relativeTime, undoneBranch, undoneCount, wentTo, windowRows } from './history-view';
import type { Revision } from './types';

const NOW = Date.parse('2026-11-22T12:00:00Z');
const rev = (seq: number, over: Partial<Revision> = {}): Revision => ({
	seq,
	label: `Edit ${seq}`,
	source: 'user',
	created_at: '2026-11-22T11:59:00Z',
	current: false,
	...over
});

describe('how long ago', () => {
	test('reads in the unit that matters', () => {
		const at = (ms: number) => new Date(NOW - ms).toISOString();
		expect(relativeTime(at(5_000), NOW)).toBe('just now');
		expect(relativeTime(at(44_000), NOW)).toBe('just now');
		expect(relativeTime(at(60_000), NOW)).toBe('1 min ago');
		expect(relativeTime(at(4 * 60_000), NOW)).toBe('4 min ago');
		expect(relativeTime(at(59 * 60_000), NOW)).toBe('59 min ago');
		expect(relativeTime(at(3 * 3_600_000), NOW)).toBe('3 h ago');
	});

	test('past a day it is a date, and a time that is not one says nothing', () => {
		expect(relativeTime(new Date(NOW - 3 * 86_400_000).toISOString(), NOW)).not.toContain('ago');
		expect(relativeTime('not a time', NOW)).toBe('');
		// a clock a little ahead of the project's is not negative time
		expect(relativeTime(new Date(NOW + 5_000).toISOString(), NOW)).toBe('just now');
	});
});

describe('the exact time of a revision', () => {
	test('is read in the user’s own format, and a stamp that is not a time is shown as it came', () => {
		expect(absoluteTime('2026-11-22T11:59:00Z')).not.toContain('T11');
		expect(absoluteTime('2026-11-22T11:59:00Z')).toContain('2026');
		expect(absoluteTime('not a time')).toBe('not a time');
	});
});

describe('the rows of the history', () => {
	test('newest first, with who made each and where it stands against the head', () => {
		const rows = historyRows([rev(0, { source: 'system', label: 'Initial state' }), rev(1, { source: 'agent' }), rev(2), rev(3, { current: true }), rev(4)]);
		expect(rows.map((r) => r.seq)).toEqual([4, 3, 2, 1, 0]);
		expect(rows.map((r) => r.state)).toEqual(['undone', 'current', 'past', 'past', 'past']);
		expect(rows.map((r) => r.who)).toEqual(['You', 'You', 'You', 'Agent', 'Kerf']);
		// the row carries when it was made; the words are made from it as time passes, not when the rows are
		expect(rows[1].iso).toBe('2026-11-22T11:59:00Z');
		expect(relativeTime(rows[1].iso, NOW)).toBe('1 min ago');
	});

	test('the user’s and the agent’s are told apart by name as well as by tint', () => {
		const rows = historyRows([rev(1, { source: 'agent' }), rev(2, { source: 'user', current: true })]);
		expect(rows.map((r) => [r.source, r.who])).toEqual([
			['user', 'You'],
			['agent', 'Agent']
		]);
	});

	test('with no head in the page every row is after it', () => {
		expect(historyRows([rev(5), rev(6)]).map((r) => r.state)).toEqual(['undone', 'undone']);
	});

	test('an empty history is no rows', () => {
		expect(historyRows([])).toEqual([]);
	});
});

describe('the edits after the head are what the next edit drops', () => {
	test('which edits, how many, and whether the count is the whole of it', () => {
		const h = [rev(1), rev(2, { current: true, label: 'Split clip' }), rev(3, { label: 'Remove clip' }), rev(4, { label: 'Trim clip' })];
		expect(undoneBranch(h)).toEqual({ count: 2, labels: ['Remove clip', 'Trim clip'], exact: true });
		expect(undoneCount(h)).toBe(2);
		expect(undoneBranch([rev(1), rev(2, { current: true })])).toEqual({ count: 0, labels: [], exact: true });
		expect(undoneBranch([])).toEqual({ count: 0, labels: [], exact: true });
	});

	test('a head older than the page read leaves every row after it, and says the count is at least that', () => {
		const b = undoneBranch([rev(7), rev(8), rev(9)]);
		expect(b.count).toBe(3);
		expect(b.exact).toBe(false);
		expect(b.labels).toEqual(['Edit 7', 'Edit 8', 'Edit 9']);
	});

	test('in a sentence: one, two, a few, and many', () => {
		expect(branchSentence([])).toBe('');
		expect(branchSentence(['Remove clip'])).toBe('“Remove clip”');
		expect(branchSentence(['A', 'B'])).toBe('“A” and “B”');
		expect(branchSentence(['A', 'B', 'C'])).toBe('“A”, “B” and “C”');
		expect(branchSentence(['A', 'B', 'C', 'D', 'E'])).toBe('“A”, “B”, “C” and 2 more');
	});

	test('the note over the list says how far back and for how long the rest is kept — until the next edit, an apply included', () => {
		expect(backNote(0)).toBeNull();
		expect(backNote(1)).toBe('You are 1 edit back. What comes after this point is kept only until you make a new edit; applying an agent’s proposal is one, and it drops it.');
		expect(backNote(3)).toContain('You are 3 edits back.');
		expect(backNote(3)).toContain('drops them');
		expect(backNote({ count: 5, labels: [], exact: false })).toContain('You are at least 5 edits back.');
	});
});

describe('going back is a move, kept only until the next edit', () => {
	test('a row’s button is Go back for the past and Redo to here for what was undone', () => {
		expect(goLabel('past')).toBe('Go back');
		expect(goLabel('undone')).toBe('Redo to here');
		expect(goTip('past', 2)).toContain('The 2 edits after it are kept only until you make a new edit');
		expect(goTip('past', 1)).toContain('The 1 edit after it is kept only until');
		expect(goTip('past', 0)).toBe('Put the cut back as it was at this edit');
		expect(goTip('undone', 0)).toContain('forward');
	});

	test('the button is named with its revision and its time, so a list of them is not a list of "Go back"', () => {
		const name = goName({ state: 'past', label: 'Split clip', iso: '2026-11-22T11:59:00Z' });
		expect(name.startsWith('Go back to: Split clip, ')).toBe(true);
		expect(name).toMatch(/\d{1,2}[:.]\d{2}/);
		expect(goName({ state: 'undone', label: 'Trim', iso: '2026-11-22T11:59:00Z' }).startsWith('Redo to: Trim, ')).toBe(true);
		expect(goName({ state: 'past', label: 'Trim', iso: 'nope' })).toBe('Go back to: Trim');
	});

	test('the notice says where the cut is now and that what follows is kept only until the next edit', () => {
		expect(wentTo('Split clip', 'past', 2)).toEqual({ text: 'Went back to “Split clip”', description: 'The 2 later edits are kept only until you make a new edit.' });
		expect(wentTo('Split clip', 'past', 1).description).toBe('The later edit is kept only until you make a new edit.');
		expect(wentTo('Split clip', 'past', 0).description).toBeNull();
		expect(wentTo('Trim', 'undone', 3)).toEqual({ text: 'Went forward to “Trim”', description: null });
	});
});

describe('the list is windowed, and the head is always in the window', () => {
	const rows = (n: number, currentAt = n - 1) => Array.from({ length: n }, (_, i) => ({ state: (i === currentAt ? 'current' : i < currentAt ? 'undone' : 'past') as 'current' | 'undone' | 'past', i }));

	test('the newest rows are drawn and the rest are counted', () => {
		const w = windowRows(rows(300, 0), 40);
		expect(w.rows).toHaveLength(40);
		expect(w.more).toBe(260);
		expect(windowRows(rows(10, 0), 40)).toEqual({ rows: rows(10, 0), more: 0 });
	});

	test('a cut stepped far back has its current marker drawn, with a little after it', () => {
		const w = windowRows(rows(300, 120), 40);
		expect(w.rows.findIndex((r) => r.state === 'current')).toBe(120);
		expect(w.rows).toHaveLength(123);
		expect(w.more).toBe(177);
	});

	test('a page with no current row is windowed by the count alone', () => {
		expect(windowRows(rows(100, -1).map((r) => ({ ...r, state: 'undone' as const })), 40).rows).toHaveLength(40);
	});

	test('the page is widened to find the head only while it is missing and older ones exist', () => {
		expect(needsOlderForHead([rev(8), rev(9)], true)).toBe(true);
		expect(needsOlderForHead([rev(8), rev(9)], false)).toBe(false);
		expect(needsOlderForHead([rev(8), rev(9, { current: true })], true)).toBe(false);
		expect(needsOlderForHead([], true)).toBe(false);
	});
});
