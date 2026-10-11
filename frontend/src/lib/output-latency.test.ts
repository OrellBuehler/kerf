import { describe, expect, test } from 'bun:test';
import { MAX_OUTPUT_LATENCY, outputLatency } from './output-latency';

describe('outputLatency', () => {
	test('is how far the render head is ahead of the sample the device plays now', () => {
		const ctx = { currentTime: 10.05, getOutputTimestamp: () => ({ contextTime: 10.0, performanceTime: 1 }), outputLatency: 0.2 };
		expect(outputLatency(ctx)).toBeCloseTo(0.05, 9);
	});

	test('falls back to what the browser reports when there is no timestamp yet', () => {
		// contextTime 0: the device has not started.
		const ctx = {
			currentTime: 3,
			getOutputTimestamp: () => ({ contextTime: 0, performanceTime: 0 }),
			outputLatency: 0.03,
			baseLatency: 0.01
		};
		expect(outputLatency(ctx)).toBeCloseTo(0.04, 9);
		// Safari: a base latency alone.
		expect(outputLatency({ currentTime: 3, baseLatency: 0.012 })).toBeCloseTo(0.012, 9);
		// A timestamp ahead of the render head is no delay: the reported latency is read instead.
		expect(
			outputLatency({ currentTime: 3, getOutputTimestamp: () => ({ contextTime: 3.2 }), outputLatency: 0.02 })
		).toBeCloseTo(0.02, 9);
	});

	test('is 0 for a context that cannot say, and for no context', () => {
		expect(outputLatency(null)).toBe(0);
		expect(outputLatency(undefined)).toBe(0);
		expect(outputLatency({ currentTime: 5 })).toBe(0);
		expect(outputLatency({ currentTime: 5, outputLatency: Number.NaN, baseLatency: -1 })).toBe(0);
	});

	test('a context that throws for a timestamp is read the other way', () => {
		const ctx = {
			currentTime: 1,
			getOutputTimestamp: () => {
				throw new Error('not running');
			},
			outputLatency: 0.04
		};
		expect(outputLatency(ctx)).toBeCloseTo(0.04, 9);
	});

	test('is held to what is believable', () => {
		expect(outputLatency({ currentTime: 100, getOutputTimestamp: () => ({ contextTime: 1 }) })).toBe(MAX_OUTPUT_LATENCY);
		expect(outputLatency({ currentTime: 1, outputLatency: 9 })).toBe(MAX_OUTPUT_LATENCY);
	});
});
