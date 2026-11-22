import { describe, expect, test } from 'bun:test';
import { EDITED_SINCE, headRef, holdsRevision, sameRevision, undoVerdict } from './revision-ref';

const r = (seq: number, created_at: string, current = false) => ({ seq, created_at, current });

describe('naming a revision', () => {
	test('the head is the current revision, by number and time', () => {
		expect(headRef([r(1, 'a'), r(2, 'b', true), r(3, 'c')])).toEqual({ seq: 2, created_at: 'b' });
		expect(headRef([r(1, 'a')])).toBeNull();
		expect(headRef([])).toBeNull();
	});

	test('a number alone is not a name: after an undo and a new edit another revision has it', () => {
		const was = { seq: 5, created_at: '10:00' };
		expect(sameRevision(was, { seq: 5, created_at: '10:00' })).toBe(true);
		expect(sameRevision(was, { seq: 5, created_at: '10:07' })).toBe(false);
		expect(sameRevision(was, { seq: 6, created_at: '10:00' })).toBe(false);
		expect(sameRevision(was, null)).toBe(false);
		expect(sameRevision(null, null)).toBe(false);
		expect(holdsRevision([r(5, '10:00')], was)).toBe(true);
		expect(holdsRevision([r(5, '10:07')], was)).toBe(false);
		expect(holdsRevision([r(5, '10:00')], null)).toBe(false);
	});
});

describe('what an Undo that captured a revision may do', () => {
	const mine = { seq: 3, created_at: 'c' };

	test('undo while it is still the head', () => {
		expect(undoVerdict(mine, [r(2, 'b'), r(3, 'c', true)])).toBe('undo');
	});

	test('nothing once the cut was edited since: the head is another revision, or the same number made later', () => {
		expect(undoVerdict(mine, [r(2, 'b'), r(3, 'c'), r(4, 'd', true)])).toBe('edited');
		expect(undoVerdict(mine, [r(2, 'b'), r(3, 'c2', true)])).toBe('edited');
		expect(undoVerdict(mine, [r(2, 'b', true), r(3, 'c')])).toBe('edited');
	});

	test('nothing when the head cannot be told', () => {
		expect(undoVerdict(mine, [r(8, 'x'), r(9, 'y')])).toBe('unknown');
	});

	test('the refusal says why and where to go', () => {
		expect(EDITED_SINCE).toContain('edited since');
		expect(EDITED_SINCE).toContain('History');
	});
});
