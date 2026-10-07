import { describe, expect, test } from 'bun:test';
import { afterPaint, PAINT_TIMEOUT_MS, revealWindow, type PaintClock } from './reveal';

/** A clock the test drives by hand. */
function manualClock() {
	const frames: Array<() => void> = [];
	const timers: Array<{ cb: () => void; ms: number }> = [];
	const clock: PaintClock = {
		frame: (cb) => void frames.push(cb),
		timeout: (cb, ms) => void timers.push({ cb, ms })
	};
	return {
		clock,
		timers,
		/** Run the callbacks queued so far (the next frame). */
		nextFrame() {
			frames.splice(0).forEach((cb) => cb());
		},
		fireTimers() {
			timers.splice(0).forEach((t) => t.cb());
		}
	};
}

/** Whether a promise has settled, without waiting for one that never will. */
async function settled(p: Promise<unknown>): Promise<boolean> {
	let done = false;
	void p.then(() => (done = true));
	await Promise.resolve();
	await Promise.resolve();
	return done;
}

describe('afterPaint', () => {
	test('waits for two frames: the one before a paint and the one after it', async () => {
		const t = manualClock();
		const p = afterPaint(t.clock);
		expect(await settled(p)).toBe(false);
		t.nextFrame();
		expect(await settled(p)).toBe(false);
		t.nextFrame();
		expect(await settled(p)).toBe(true);
	});

	test('does not wait forever on a page that is never given a frame', async () => {
		// A window that is not shown does not paint: no frame ever comes.
		const t = manualClock();
		const p = afterPaint(t.clock);
		expect(await settled(p)).toBe(false);
		expect(t.timers.map((x) => x.ms)).toEqual([PAINT_TIMEOUT_MS]);
		t.fireTimers();
		expect(await settled(p)).toBe(true);
	});

	test('resolves once however many of the two fire', async () => {
		const t = manualClock();
		let n = 0;
		const p = afterPaint(t.clock).then(() => n++);
		t.nextFrame();
		t.nextFrame();
		t.fireTimers();
		await p;
		await Promise.resolve();
		expect(n).toBe(1);
	});
});

describe('revealWindow', () => {
	test('settles, then paints, then shows', async () => {
		const order: string[] = [];
		const ok = await revealWindow({
			settle: async () => void order.push('settle'),
			paint: async () => void order.push('paint'),
			show: async () => void order.push('show')
		});
		expect(ok).toBe(true);
		expect(order).toEqual(['settle', 'paint', 'show']);
	});

	test('does not show until the paint has resolved', async () => {
		let release!: () => void;
		let shown = false;
		const p = revealWindow({
			settle: async () => {},
			paint: () => new Promise<void>((r) => (release = r)),
			show: async () => void (shown = true)
		});
		await Promise.resolve();
		await Promise.resolve();
		expect(shown).toBe(false);
		release();
		expect(await p).toBe(true);
		expect(shown).toBe(true);
	});

	test('a failed show reports false instead of rejecting', async () => {
		const ok = await revealWindow({
			settle: async () => {},
			paint: async () => {},
			show: async () => {
				throw new Error('no window');
			}
		});
		expect(ok).toBe(false);
	});

	test('a failed step before the show does not keep the window hidden', async () => {
		let shown = false;
		const ok = await revealWindow({
			settle: async () => {
				throw new Error('boom');
			},
			paint: async () => {},
			show: async () => void (shown = true)
		});
		expect(ok).toBe(true);
		expect(shown).toBe(true);
	});
});
