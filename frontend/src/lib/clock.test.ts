import { describe, expect, test } from 'bun:test';
import { clock, clockRange } from './clock';
import { formatTime } from './diff';

describe('clock', () => {
	test('reads a position as m:ss.d', () => {
		expect(clock(0)).toBe('0:00.0');
		expect(clock(12.5)).toBe('0:12.5');
		expect(clock(65.04)).toBe('1:05.0');
		expect(clock(600)).toBe('10:00.0');
	});

	test('carries a rounded tenth into the next unit instead of printing 60', () => {
		expect(clock(59.96)).toBe('1:00.0');
		expect(clock(119.99)).toBe('2:00.0');
		expect(clock(3599.97)).toBe('1:00:00.0');
	});

	test('counts hours from the sixtieth minute', () => {
		expect(clock(3600)).toBe('1:00:00.0');
		expect(clock(3725.5)).toBe('1:02:05.5');
	});

	test('a negative, NaN or infinite time is zero', () => {
		for (const t of [-3, NaN, Infinity, -Infinity]) expect(clock(t)).toBe('0:00.0');
	});

	test('agrees with the review card on every ordinary position', () => {
		for (const t of [0, 0.5, 3.2, 12.5, 59.4, 61.7, 125.3, 599.9]) expect(clock(t)).toBe(formatTime(t));
	});

	test('a range is two positions and an en dash', () => {
		expect(clockRange(12.5, 20)).toBe('0:12.5 – 0:20.0');
	});
});
