import { describe, expect, test } from 'bun:test';
import { frameLimited, scrubFraction, scrubRange, scrubTime, type FrameClock } from './scrub';

describe('scrubTime', () => {
	test('is the pointer\'s share of the bar, times the cut', () => {
		expect(scrubTime(100, 100, 400, 20)).toBe(0);
		expect(scrubTime(300, 100, 400, 20)).toBe(10);
		expect(scrubTime(500, 100, 400, 20)).toBe(20);
		expect(scrubTime(150, 100, 400, 20)).toBeCloseTo(2.5, 12);
	});

	test('holds a pointer that has left the bar to its ends, so a drag can overshoot', () => {
		expect(scrubTime(-50, 100, 400, 20)).toBe(0);
		expect(scrubTime(5000, 100, 400, 20)).toBe(20);
	});

	test('a bar or a cut with no size, or a coordinate that is not a number, is time zero', () => {
		expect(scrubTime(200, 100, 0, 20)).toBe(0);
		expect(scrubTime(200, 100, 400, 0)).toBe(0);
		expect(scrubTime(NaN, 100, 400, 20)).toBe(0);
		expect(scrubTime(200, NaN, 400, 20)).toBe(0);
	});
});

describe('scrubFraction and scrubRange', () => {
	test('the thumb is at the playhead\'s share of the cut, held inside the bar', () => {
		expect(scrubFraction(5, 20)).toBe(0.25);
		expect(scrubFraction(-1, 20)).toBe(0);
		expect(scrubFraction(25, 20)).toBe(1);
		expect(scrubFraction(5, 0)).toBe(0);
		expect(scrubFraction(NaN, 20)).toBe(0);
	});

	test('the range is the marks, when both are set and enclose something', () => {
		expect(scrubRange(5, 15, 20)).toEqual({ from: 0.25, to: 0.75 });
		expect(scrubRange(null, 15, 20)).toBeNull();
		expect(scrubRange(5, null, 20)).toBeNull();
		expect(scrubRange(15, 5, 20)).toBeNull();
		expect(scrubRange(5, 5, 20)).toBeNull();
		expect(scrubRange(5, 15, 0)).toBeNull();
	});
});

/** A clock the test turns by hand. */
function clock() {
	const queue = new Map<number, () => void>();
	let next = 1;
	const c: FrameClock = {
		request: (cb) => {
			queue.set(next, cb);
			return next++;
		},
		cancel: (id) => void queue.delete(id)
	};
	return {
		c,
		get pending() {
			return queue.size;
		},
		frame() {
			const run = [...queue.values()];
			queue.clear();
			for (const cb of run) cb();
		}
	};
}

describe('frameLimited', () => {
	test('applies once per frame however many positions arrive in it, and the newest', () => {
		const seen: number[] = [];
		const k = clock();
		const f = frameLimited((v: number) => seen.push(v), k.c);
		for (let x = 1; x <= 50; x++) f.push(x);
		expect(k.pending).toBe(1);
		expect(seen).toEqual([]);
		k.frame();
		expect(seen).toEqual([50]);
		f.push(51);
		f.push(52);
		k.frame();
		expect(seen).toEqual([50, 52]);
	});

	test('a frame with nothing new applies nothing', () => {
		const seen: number[] = [];
		const k = clock();
		const f = frameLimited((v: number) => seen.push(v), k.c);
		f.push(1);
		k.frame();
		k.frame();
		expect(seen).toEqual([1]);
	});

	test('flush applies what is waiting now and leaves nothing for the frame', () => {
		const seen: number[] = [];
		const k = clock();
		const f = frameLimited((v: number) => seen.push(v), k.c);
		f.push(7);
		f.flush();
		expect(seen).toEqual([7]);
		expect(k.pending).toBe(0);
		k.frame();
		expect(seen).toEqual([7]);
		f.flush();
		expect(seen).toEqual([7]);
	});

	test('cancel forgets what is waiting: a drag given up does not move the playhead after the fact', () => {
		const seen: number[] = [];
		const k = clock();
		const f = frameLimited((v: number) => seen.push(v), k.c);
		f.push(9);
		expect(f.waiting).toBe(true);
		f.cancel();
		expect(f.waiting).toBe(false);
		expect(k.pending).toBe(0);
		k.frame();
		expect(seen).toEqual([]);
		// and it works again afterwards
		f.push(3);
		k.frame();
		expect(seen).toEqual([3]);
	});
});
