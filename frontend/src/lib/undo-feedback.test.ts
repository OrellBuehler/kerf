import { describe, expect, test } from 'bun:test';
import { BURST_MS, burstLine, nextBurst, redoLabel, undoLabel, withLabel, type Burst } from './undo-feedback';

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
		expect(first).toEqual({ kind: 'undo', count: 1, first: 'Split clip', last: 'Split clip', at: 1000 });
		expect(nextBurst(first, 'undo', 'Trim', 1000 + BURST_MS + 1).count).toBe(1);
		expect(nextBurst(first, 'redo', 'Split clip', 1100).count).toBe(1);
	});

	test('presses within the window count up and remember where they have got to', () => {
		let b: Burst = nextBurst(null, 'undo', 'Ripple delete', 0);
		b = nextBurst(b, 'undo', 'Split clip', 300);
		b = nextBurst(b, 'undo', 'Import', 900);
		expect(b).toEqual({ kind: 'undo', count: 3, first: 'Ripple delete', last: 'Import', at: 900 });
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
		expect(burstLine({ kind: 'undo', count: 1, first: 'Split clip', last: 'Split clip', at: 0 })).toBe('Undid: Split clip');
		expect(burstLine({ kind: 'redo', count: 1, first: 'Split clip', last: 'Split clip', at: 0 })).toBe('Redid: Split clip');
		expect(burstLine({ kind: 'undo', count: 3, first: 'Ripple delete', last: 'Import', at: 0 })).toBe('Undid 3 edits, back to before Import');
		expect(burstLine({ kind: 'redo', count: 2, first: 'Import', last: 'Trim', at: 0 })).toBe('Redid 2 edits, up to Trim');
		expect(burstLine({ kind: 'undo', count: 1, first: null, last: null, at: 0 })).toBe('Undid');
		expect(burstLine({ kind: 'undo', count: 2, first: null, last: null, at: 0 })).toBe('Undid 2 edits');
	});
});
