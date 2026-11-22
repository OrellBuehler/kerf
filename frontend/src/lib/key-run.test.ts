import { describe, expect, test } from 'bun:test';
import { KeyRun, type Timers } from './key-run';

/** Timers that run when the test says so. */
function clock() {
	let now = 0;
	let next = 1;
	const waiting = new Map<number, { at: number; fn: () => void }>();
	const timers: Timers = {
		set: (fn, ms) => {
			const id = next++;
			waiting.set(id, { at: now + ms, fn });
			return id;
		},
		clear: (h) => void waiting.delete(h as number)
	};
	return {
		timers,
		pending: () => waiting.size,
		advance(ms: number) {
			now += ms;
			for (const [id, w] of [...waiting]) {
				if (w.at <= now) {
					waiting.delete(id);
					w.fn();
				}
			}
		}
	};
}

describe('a run of key presses', () => {
	test('is one edit, written when the keys have been quiet', () => {
		const c = clock();
		const done: [number, number][] = [];
		const run = new KeyRun<number>((from, last) => done.push([from, last]), 450, c.timers);
		run.push(0, 1);
		c.advance(200);
		run.push(99, 2);
		c.advance(200);
		run.push(99, 3);
		c.advance(449);
		expect(done).toEqual([]);
		c.advance(1);
		expect(done).toEqual([[0, 3]]);
		expect(run.active).toBe(false);
	});

	test('begins where the first press began, whatever the later ones say', () => {
		const c = clock();
		const run = new KeyRun<number>(() => {}, 450, c.timers);
		run.push(5, 6);
		run.push(0, 7);
		expect(run.from).toBe(5);
		expect(run.last).toBe(7);
	});

	test('finish writes it now, and once', () => {
		const c = clock();
		const done: [string, string][] = [];
		const run = new KeyRun<string>((a, b) => done.push([a, b]), 450, c.timers);
		run.push('a', 'b');
		run.finish();
		run.finish();
		c.advance(1000);
		expect(done).toEqual([['a', 'b']]);
		expect(c.pending()).toBe(0);
	});

	test('cancel writes nothing and says whether there was a run', () => {
		const c = clock();
		const done: number[] = [];
		const run = new KeyRun<number>((_, last) => done.push(last), 450, c.timers);
		expect(run.cancel()).toBe(false);
		run.push(0, 1);
		expect(run.cancel()).toBe(true);
		c.advance(1000);
		expect(done).toEqual([]);
		expect(run.active).toBe(false);
		expect(run.last).toBeNull();
		expect(c.pending()).toBe(0);
	});

	test('a new run after one has ended starts fresh', () => {
		const c = clock();
		const done: [number, number][] = [];
		const run = new KeyRun<number>((a, b) => done.push([a, b]), 450, c.timers);
		run.push(0, 1);
		run.finish();
		run.push(1, 2);
		run.finish();
		expect(done).toEqual([
			[0, 1],
			[1, 2]
		]);
	});

	test('a write that starts a run of its own from its callback is not lost', () => {
		const c = clock();
		const done: number[] = [];
		const run: KeyRun<number> = new KeyRun<number>((_, last) => {
			done.push(last);
			if (last === 1) run.push(1, 2);
		}, 450, c.timers);
		run.push(0, 1);
		run.finish();
		expect(run.active).toBe(true);
		run.finish();
		expect(done).toEqual([1, 2]);
	});
});
