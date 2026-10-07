import { describe, expect, test } from 'bun:test';
import { Backoff } from './backoff';

/** A clock the test moves by hand. */
function clock(start = 1_000) {
	let t = start;
	return { now: () => t, advance: (ms: number) => (t += ms) };
}

describe('Backoff', () => {
	test('a key nobody failed is not held off', () => {
		const b = new Backoff();
		expect(b.retryIn('a')).toBeUndefined();
		expect(b.failure('a')).toBeUndefined();
		expect(b.count('a')).toBe(0);
	});

	test('a failure is held against the key for the cooldown, then forgiven', () => {
		const c = clock();
		const b = new Backoff({ baseMs: 1000, now: c.now });
		b.fail('a', 'no such file');
		expect(b.failure('a')).toBe('no such file');
		expect(b.retryIn('a')).toBe(1000);
		c.advance(400);
		expect(b.retryIn('a')).toBe(600);
		c.advance(600);
		expect(b.retryIn('a')).toBeUndefined();
		expect(b.failure('a')).toBeUndefined();
	});

	test('each failure in a row doubles the cooldown, up to the cap', () => {
		const c = clock();
		const b = new Backoff({ baseMs: 1000, maxMs: 5000, now: c.now });
		const waits: number[] = [];
		for (let i = 0; i < 5; i++) {
			b.fail('a', 'x');
			waits.push(b.retryIn('a')!);
			c.advance(b.retryIn('a')!); // wait it out, then it fails again
		}
		expect(waits).toEqual([1000, 2000, 4000, 5000, 5000]);
		expect(b.count('a')).toBe(5);
	});

	test('failures that land together are one failure, not a faster climb', () => {
		const c = clock();
		const b = new Backoff({ baseMs: 1000, now: c.now });
		b.fail('a', 'first');
		c.advance(10);
		b.fail('a', 'second of the same burst');
		b.fail('a', 'third');
		expect(b.count('a')).toBe(1);
		expect(b.retryIn('a')).toBe(990); // still timed from the first
	});

	test('success wipes the record, so the next failure starts from the first cooldown', () => {
		const c = clock();
		const b = new Backoff({ baseMs: 1000, now: c.now });
		b.fail('a', 'x');
		c.advance(1000);
		b.fail('a', 'x');
		expect(b.count('a')).toBe(2);
		b.succeed('a');
		expect(b.count('a')).toBe(0);
		b.fail('a', 'x');
		expect(b.retryIn('a')).toBe(1000);
	});

	test('keys are independent, and retain keeps only the ones named', () => {
		const b = new Backoff();
		b.fail('a', 'x');
		b.fail('b', 'y');
		expect(b.failure('a')).toBe('x');
		expect(b.failure('b')).toBe('y');
		b.retain(['b']);
		expect(b.failure('a')).toBeUndefined();
		expect(b.failure('b')).toBe('y');
		b.clear();
		expect(b.failure('b')).toBeUndefined();
	});

	test('the defaults are the waveform cache\'s: 30 s, doubling, ten minutes at most', () => {
		const b = new Backoff();
		expect(b.cooldown(1)).toBe(30_000);
		expect(b.cooldown(2)).toBe(60_000);
		expect(b.cooldown(20)).toBe(600_000);
	});
});
