import { describe, expect, test } from 'bun:test';
import { absoluteTime, backNote, goLabel, goTip, historyRows, relativeTime, undoneCount, wentTo } from './history-view';
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
		const rows = historyRows([rev(0, { source: 'system', label: 'Initial state' }), rev(1, { source: 'agent' }), rev(2), rev(3, { current: true }), rev(4)], NOW);
		expect(rows.map((r) => r.seq)).toEqual([4, 3, 2, 1, 0]);
		expect(rows.map((r) => r.state)).toEqual(['undone', 'current', 'past', 'past', 'past']);
		expect(rows.map((r) => r.who)).toEqual(['You', 'You', 'You', 'Agent', 'Kerf']);
		expect(rows[1].when).toBe('1 min ago');
	});

	test('the user’s and the agent’s are told apart by name as well as by tint', () => {
		const rows = historyRows([rev(1, { source: 'agent' }), rev(2, { source: 'user', current: true })], NOW);
		expect(rows.map((r) => [r.source, r.who])).toEqual([
			['user', 'You'],
			['agent', 'Agent']
		]);
	});

	test('with no head in the page every row is after it', () => {
		expect(historyRows([rev(5), rev(6)], NOW).map((r) => r.state)).toEqual(['undone', 'undone']);
	});

	test('an empty history is no rows', () => {
		expect(historyRows([], NOW)).toEqual([]);
	});
});

describe('going back is a move, not a loss', () => {
	test('how many are undone, and the note that says a new edit drops them', () => {
		expect(undoneCount([rev(1), rev(2, { current: true })])).toBe(0);
		expect(undoneCount([rev(1), rev(2, { current: true }), rev(3), rev(4)])).toBe(2);
		expect(undoneCount([rev(1), rev(2)])).toBe(0);
		expect(backNote(0)).toBeNull();
		expect(backNote(1)).toBe('You are 1 edit back. The edit after this point is kept until you make a new edit — a new edit drops it.');
		expect(backNote(3)).toBe('You are 3 edits back. The 3 edits after this point are kept until you make a new edit — a new edit drops them.');
	});

	test('a row’s button is Go back for the past and Redo to here for what was undone', () => {
		expect(goLabel('past')).toBe('Go back');
		expect(goLabel('undone')).toBe('Redo to here');
		expect(goTip('past', 2)).toContain('The 2 edits after it are kept');
		expect(goTip('past', 1)).toContain('The 1 edit after it is kept');
		expect(goTip('past', 0)).toBe('Put the cut back as it was at this edit');
		expect(goTip('undone', 0)).toContain('forward');
	});

	test('the notice says where the cut is now and that what follows is kept', () => {
		expect(wentTo('Split clip', 'past', 2)).toEqual({ text: 'Went back to “Split clip”', description: 'The 2 later edits are kept until you make a new edit.' });
		expect(wentTo('Split clip', 'past', 1).description).toBe('The later edit is kept until you make a new edit.');
		expect(wentTo('Split clip', 'past', 0).description).toBeNull();
		expect(wentTo('Trim', 'undone', 3)).toEqual({ text: 'Went forward to “Trim”', description: null });
	});
});
