import { describe, expect, test } from 'bun:test';
import { headStamp, undoVerdict } from './undo-toast';

const rev = (seq: number, at: string, current = false) => ({ seq, created_at: at, current });

describe('headStamp', () => {
	test('is the current revision’s number and time', () => {
		expect(headStamp([rev(0, 't0'), rev(1, 't1', true)])).toEqual({ seq: 1, at: 't1' });
		expect(headStamp([])).toBeUndefined();
		expect(headStamp([rev(0, 't0')])).toBeUndefined();
	});
});

describe('undoVerdict', () => {
	test('the edit is the current revision: undo it', () => {
		expect(undoVerdict({ seq: 2, at: 't2' }, [rev(1, 't1'), rev(2, 't2', true)])).toBe('undo');
	});

	test('edits on top of it: moved on', () => {
		expect(undoVerdict({ seq: 1, at: 't1' }, [rev(1, 't1'), rev(2, 't2', true)])).toBe('moved-on');
	});

	test('it is undone and sits in the redo branch: already undone', () => {
		expect(undoVerdict({ seq: 2, at: 't2' }, [rev(1, 't1', true), rev(2, 't2')])).toBe('undone');
	});

	test('undone, then another edit took its number: not the same edit — already undone', () => {
		expect(undoVerdict({ seq: 1, at: 'tA' }, [rev(0, 't0'), rev(1, 'tB', true)])).toBe('undone');
		expect(undoVerdict({ seq: 1, at: 'tA' }, [rev(0, 't0'), rev(1, 'tB'), rev(2, 'tC', true)])).toBe('undone');
	});

	test('undone and dropped from the redo branch by an edit that took a lower number', () => {
		expect(undoVerdict({ seq: 5, at: 't5' }, [rev(3, 't3'), rev(4, 't4', true)])).toBe('undone');
	});

	test('older than the page of history held: edits are on top of it', () => {
		expect(undoVerdict({ seq: 5, at: 't5' }, [rev(10, 't10'), rev(11, 't11', true)])).toBe('moved-on');
	});

	test('no history when the edit landed: fine while there still is none, else conservative', () => {
		expect(undoVerdict(undefined, [])).toBe('undo');
		expect(undoVerdict(undefined, [rev(1, 't1', true)])).toBe('moved-on');
		expect(undoVerdict({ seq: 1, at: 't1' }, [])).toBe('moved-on');
	});
});
