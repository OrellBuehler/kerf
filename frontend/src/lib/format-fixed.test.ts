import { describe, expect, test } from 'bun:test';
import { toFixedEven } from './format-fixed';

describe('toFixedEven', () => {
	test('is toFixed everywhere but an exact tie', () => {
		for (const [x, d, want] of [
			[0, 1, '0.0'],
			[4, 1, '4.0'],
			[3.14159, 2, '3.14'],
			[0.35, 1, '0.3'], // a hair under a tie
			[0.45, 1, '0.5'], // a hair over one
			[1.005, 2, '1.00'], // 1.00499999999999989…
			[2.675, 2, '2.67'],
			[-0.04, 1, '-0.0'],
			[-0, 1, '-0.0'], // Rust keeps the sign of a negative zero
			[123456.789, 2, '123456.79']
		] as const)
			expect(toFixedEven(x, d)).toBe(want);
	});

	test('takes an exact tie to the even digit, up or down', () => {
		for (const [x, d, want] of [
			[0.25, 1, '0.2'],
			[0.75, 1, '0.8'],
			[4.25, 1, '4.2'],
			[4.75, 1, '4.8'],
			[0.125, 2, '0.12'],
			[0.375, 2, '0.38'],
			[0.0625, 3, '0.062'],
			[0.1875, 3, '0.188'],
			[-4.25, 1, '-4.2'],
			[-4.75, 1, '-4.8'],
			[9.75, 1, '9.8']
		] as const)
			expect(toFixedEven(x, d)).toBe(want);
	});

	test('leaves what it cannot reason about to toFixed', () => {
		expect(toFixedEven(NaN, 1)).toBe('NaN');
		expect(toFixedEven(Infinity, 2)).toBe('Infinity');
	});
});
