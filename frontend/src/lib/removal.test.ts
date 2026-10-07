import { describe, expect, test } from 'bun:test';
import { removalNotice } from './removal';

describe('removalNotice', () => {
	test('says what was removed, singular and plural', () => {
		expect(removalNotice(1, 0, false)).toBe('Clip removed');
		expect(removalNotice(4, 0, false)).toBe('4 clips removed');
	});

	test('says when the gaps were closed', () => {
		expect(removalNotice(1, 0, true)).toBe('Clip ripple-deleted');
		expect(removalNotice(3, 0, true)).toBe('3 clips ripple-deleted');
	});

	test('says what a locked track kept', () => {
		expect(removalNotice(2, 1, false)).toBe('2 clips removed · 1 on a locked track left alone');
	});
});
