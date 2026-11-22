import { describe, expect, test } from 'bun:test';
import { BURST_MS, burstLine, headSeq, isEndOfHistory, nextBurst, redoLabel, undoLabel, withLabel, type Burst } from './undo-feedback';

const rev = (label: string, current = false) => ({ label, current });

describe('what Undo and Redo are called', () => {
	test('Undo names the edit the project is at; Redo the one after it', () => {
		const h = [rev('Import'), rev('Split clip'), rev('Ripple delete', true), rev('Add marker')];
		expect(undoLabel(h)).toBe('Ripple delete');
		expect(redoLabel(h)).toBe('Add marker');
	});

	test('nothing to undo at the oldest edit, unless older ones exist beyond the page', () => {
		const h = [rev('Import', true), rev('Split clip')];
		expect(undoLabel(h)).toBeNull();
		expect(undoLabel(h, true)).toBe('Import');
		expect(redoLabel(h)).toBe('Split clip');
	});

	test('nothing to redo at the newest edit; an empty history names nothing', () => {
		expect(redoLabel([rev('a'), rev('b', true)])).toBeNull();
		expect(undoLabel([])).toBeNull();
		expect(redoLabel([])).toBeNull();
	});

	test('a head older than the whole page names nothing it cannot see', () => {
		expect(undoLabel([rev('a'), rev('b')])).toBeNull();
		expect(redoLabel([rev('a'), rev('b')])).toBeNull();
	});

	test('a menu entry says the verb alone when there is no label', () => {
		expect(withLabel('Undo', 'Split clip')).toBe('Undo Split clip');
		expect(withLabel('Redo', null)).toBe('Redo');
		expect(withLabel('Undo', '')).toBe('Undo');
	});
});

describe('a burst of presses', () => {
	test('a press after a pause, or the other way, is a new burst', () => {
		const first = nextBurst(null, 'undo', 'Split clip', 1000);
		expect(first).toEqual({ kind: 'undo', head: null, count: 1, first: 'Split clip', last: 'Split clip', at: 1000 });
		expect(nextBurst(first, 'undo', 'Trim', 1000 + BURST_MS + 1).count).toBe(1);
		expect(nextBurst(first, 'redo', 'Split clip', 1100).count).toBe(1);
	});

	test('presses within the window count up and remember where they have got to', () => {
		let b: Burst = nextBurst(null, 'undo', 'Ripple delete', 0);
		b = nextBurst(b, 'undo', 'Split clip', 300);
		b = nextBurst(b, 'undo', 'Import', 900);
		expect(b).toEqual({ kind: 'undo', head: null, count: 3, first: 'Ripple delete', last: 'Import', at: 900 });
	});

	test('the window slides: each press holds the burst open for the next', () => {
		let b: Burst = nextBurst(null, 'undo', 'a', 0);
		for (let t = 1000; t <= 6000; t += 1000) b = nextBurst(b, 'undo', 'a', t);
		expect(b.count).toBe(7);
	});

	test('a press with no label keeps the last one it had', () => {
		const b = nextBurst(nextBurst(null, 'undo', 'Trim', 0), 'undo', null, 100);
		expect(b.last).toBe('Trim');
	});

	test('the line: one edit by name, several as a count and where it ended', () => {
		expect(burstLine({ kind: 'undo', head: null, count: 1, first: 'Split clip', last: 'Split clip', at: 0 })).toBe('Undid: Split clip');
		expect(burstLine({ kind: 'redo', head: null, count: 1, first: 'Split clip', last: 'Split clip', at: 0 })).toBe('Redid: Split clip');
		expect(burstLine({ kind: 'undo', head: null, count: 3, first: 'Ripple delete', last: 'Import', at: 0 })).toBe('Undid 3 edits, back to before Import');
		expect(burstLine({ kind: 'redo', head: null, count: 2, first: 'Import', last: 'Trim', at: 0 })).toBe('Redid 2 edits, up to Trim');
		expect(burstLine({ kind: 'undo', head: null, count: 1, first: null, last: null, at: 0 })).toBe('Undid');
		expect(burstLine({ kind: 'undo', head: null, count: 2, first: null, last: null, at: 0 })).toBe('Undid 2 edits');
	});
});

describe('another edit breaks a burst', () => {
	test('undo, edit, undo is two bursts of one — never "Undid 2 edits"', () => {
		// The project stood at 5; the first press took it to 4.
		let b = nextBurst(null, 'undo', 'Trim', 0, 5, 4);
		// An edit made revision 5 again (head 5), then the second undo began from there and went back to 4.
		b = nextBurst(b, 'undo', 'Add marker', 400, 5, 4);
		expect(b.count).toBe(1);
		expect(burstLine(b)).toBe('Undid: Add marker');
	});

	test('presses that follow one another are one burst', () => {
		let b = nextBurst(null, 'undo', 'c', 0, 5, 4);
		b = nextBurst(b, 'undo', 'b', 100, 4, 3);
		b = nextBurst(b, 'undo', 'a', 200, 3, 2);
		expect(b.count).toBe(3);
		expect(b.head).toBe(2);
	});

	test('a jump in the history (a revert) between presses ends it too', () => {
		let b = nextBurst(null, 'undo', 'c', 0, 5, 4);
		b = nextBurst(b, 'undo', 'a', 100, 2, 1);
		expect(b.count).toBe(1);
	});

	test('the revision the project is at is the one the history marks current', () => {
		expect(headSeq([{ seq: 3, current: false }, { seq: 4, current: true }, { seq: 5, current: false }])).toBe(4);
		expect(headSeq([{ seq: 3, current: false }])).toBeNull();
		expect(headSeq([])).toBeNull();
	});
});

describe('the end of the history', () => {
	test('the backend says so in two words; that is a notice, not a failure', () => {
		expect(isEndOfHistory(new Error('invalid argument: nothing to undo'))).toBe(true);
		expect(isEndOfHistory(new Error('nothing to redo'))).toBe(true);
		expect(isEndOfHistory('Nothing to Undo')).toBe(true);
		expect(isEndOfHistory(new Error('the project is locked'))).toBe(false);
	});
});
