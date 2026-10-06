import { expect, test } from 'bun:test';
import { describeError } from './log';

test('describeError reads strings, errors and objects', () => {
	expect(describeError('plain')).toBe('plain');
	expect(describeError(new Error('boom'))).toBe('boom');
	expect(describeError({ a: 1 })).toBe('{"a":1}');
	expect(describeError(undefined)).toBe('undefined');
});
