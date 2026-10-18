import { describe, expect, test } from 'bun:test';
import { HISTORY_PAGE, TASK_PAGE, keepNewest, redoable, undoable } from './paging';

const revs = (n: number, current: number) => Array.from({ length: n }, (_, i) => ({ current: i === current }));

describe('keeping the newest', () => {
	test('is the tail of an oldest-first list, or all of a short one', () => {
		expect(keepNewest([1, 2, 3, 4, 5], 2)).toEqual([4, 5]);
		expect(keepNewest([1, 2], 5)).toEqual([1, 2]);
		expect(keepNewest([], 3)).toEqual([]);
	});

	test('the pages are sized to be read at a glance', () => {
		expect(HISTORY_PAGE).toBeGreaterThanOrEqual(50);
		expect(TASK_PAGE).toBeGreaterThanOrEqual(10);
	});
});

describe('undo and redo with a page of the history', () => {
	test('a whole history reads as it always did', () => {
		expect(undoable(revs(4, 3), false)).toBe(true);
		expect(redoable(revs(4, 3))).toBe(false);
		expect(undoable(revs(4, 0), false)).toBe(false);
		expect(redoable(revs(4, 0))).toBe(true);
		expect(undoable(revs(1, 0), false)).toBe(false);
		expect(redoable(revs(1, 0))).toBe(false);
	});

	test('at the oldest edit of a page, older ones behind it are still an undo', () => {
		expect(undoable(revs(100, 0), true)).toBe(true);
		expect(undoable(revs(100, 0), false)).toBe(false);
		expect(redoable(revs(100, 0))).toBe(true);
	});

	test('a head older than the whole page has undo and redo both', () => {
		expect(undoable(revs(100, -1), true)).toBe(true);
		expect(redoable(revs(100, -1))).toBe(true);
	});

	test('nothing loaded, nothing to undo', () => {
		expect(undoable([], false)).toBe(false);
		expect(redoable([])).toBe(false);
	});
});
