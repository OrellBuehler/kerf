import { describe, expect, test } from 'bun:test';
import { missingProjectMessage, parseLaunchRequest } from './launch';

describe('parseLaunchRequest', () => {
	test('reads the two shapes the backend sends', () => {
		expect(parseLaunchRequest({ open: '/home/u/cut.kerf' })).toEqual({ open: '/home/u/cut.kerf' });
		expect(parseLaunchRequest({ missing: '/home/u/typo.kerf' })).toEqual({ missing: '/home/u/typo.kerf' });
	});

	test('nothing, or anything else, is no request', () => {
		for (const raw of [null, undefined, 'x', 3, [], {}, { open: '' }, { open: 3 }, { missing: null }, { other: '/a.kerf' }]) {
			expect(parseLaunchRequest(raw)).toBeNull();
		}
	});

	test('an unexpected extra key does not turn it into something else', () => {
		expect(parseLaunchRequest({ open: '/a.kerf', missing: '/b.kerf' })).toEqual({ open: '/a.kerf' });
	});
});

describe('missingProjectMessage', () => {
	test('names the path the way it was resolved', () => {
		expect(missingProjectMessage('/home/u/typo.kerf')).toBe('File not found: /home/u/typo.kerf');
	});
});
