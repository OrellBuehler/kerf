import { describe, expect, test } from 'bun:test';
import { moreBelow } from './scroll-cue';

describe('more below', () => {
	test('content that fits has nothing below it', () => {
		expect(moreBelow({ scrollTop: 0, clientHeight: 500, scrollHeight: 500 })).toBe(false);
		expect(moreBelow({ scrollTop: 0, clientHeight: 500, scrollHeight: 400 })).toBe(false);
	});

	test('content taller than the area has, until it is scrolled to the end', () => {
		expect(moreBelow({ scrollTop: 0, clientHeight: 500, scrollHeight: 700 })).toBe(true);
		expect(moreBelow({ scrollTop: 100, clientHeight: 500, scrollHeight: 700 })).toBe(true);
		expect(moreBelow({ scrollTop: 200, clientHeight: 500, scrollHeight: 700 })).toBe(false);
	});

	test('a sub-pixel remainder at the end is the end', () => {
		expect(moreBelow({ scrollTop: 199.4, clientHeight: 500, scrollHeight: 700 })).toBe(false);
		expect(moreBelow({ scrollTop: 190, clientHeight: 500, scrollHeight: 700 })).toBe(true);
	});

	test('an overflow of a pixel or two is not worth a cue', () => {
		expect(moreBelow({ scrollTop: 0, clientHeight: 500, scrollHeight: 501 })).toBe(false);
	});
});
