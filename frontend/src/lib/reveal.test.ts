import { describe, expect, test } from 'bun:test';
import { afterPaint, FONT_TIMEOUT_MS, fontsReady, PAINT_TIMEOUT_MS, REVEAL_FONTS, revealWindow, type FontLoader, type PaintClock } from './reveal';

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

describe('fontsReady', () => {
	/** `settled`, after the extra microtask hops `Promise.allSettled` takes. */
	async function settledSoon(p: Promise<unknown>): Promise<boolean> {
		let done = false;
		void p.then(() => (done = true));
		await new Promise((r) => setTimeout(r, 0));
		return done;
	}

	/** A font set whose loads the test releases by hand. */
	function loader() {
		const asked: string[] = [];
		const releases: Array<{ ok: () => void; fail: () => void }> = [];
		const l: FontLoader = {
			load: (f) => {
				asked.push(f);
				return new Promise((resolve, reject) => releases.push({ ok: () => resolve([]), fail: () => reject(new Error('no such face')) }));
			}
		};
		return { l, asked, releases };
	}

	test('asks for the sans, the mono and the display face', async () => {
		const f = loader();
		void fontsReady(f.l, manualClock().clock);
		expect(f.asked).toEqual([...REVEAL_FONTS]);
		expect(REVEAL_FONTS.join(' ')).toContain('Geist Variable');
		expect(REVEAL_FONTS.join(' ')).toContain('Geist Mono Variable');
		expect(REVEAL_FONTS.join(' ')).toContain('Space Grotesk Variable');
	});

	test('waits for every face', async () => {
		const f = loader();
		const t = manualClock();
		const p = fontsReady(f.l, t.clock);
		expect(await settledSoon(p)).toBe(false);
		f.releases[0].ok();
		f.releases[1].ok();
		expect(await settledSoon(p)).toBe(false);
		f.releases[2].ok();
		expect(await settledSoon(p)).toBe(true);
	});

	test('a face that fails to load does not hold the window up', async () => {
		const f = loader();
		const p = fontsReady(f.l, manualClock().clock);
		f.releases[0].fail();
		f.releases[1].ok();
		f.releases[2].ok();
		expect(await settledSoon(p)).toBe(true);
	});

	test('gives up after the timeout when a face never answers', async () => {
		const f = loader();
		const t = manualClock();
		const p = fontsReady(f.l, t.clock);
		expect(t.timers.map((x) => x.ms)).toEqual([FONT_TIMEOUT_MS]);
		expect(await settledSoon(p)).toBe(false);
		t.fireTimers();
		expect(await settledSoon(p)).toBe(true);
	});

	test('a page without a font set, or a loader that throws, is not waited on', async () => {
		expect(await settledSoon(fontsReady(undefined, manualClock().clock))).toBe(true);
		const broken: FontLoader = {
			load: () => {
				throw new Error('SecurityError');
			}
		};
		expect(await settledSoon(fontsReady(broken, manualClock().clock))).toBe(true);
	});
});

describe('revealWindow and the fonts', () => {
	test('settles, waits for the fonts, paints, then shows', async () => {
		const order: string[] = [];
		await revealWindow({
			settle: async () => void order.push('settle'),
			fonts: async () => void order.push('fonts'),
			paint: async () => void order.push('paint'),
			show: async () => void order.push('show')
		});
		expect(order).toEqual(['settle', 'fonts', 'paint', 'show']);
	});

	test('does not paint or show until the fonts have resolved', async () => {
		let release!: () => void;
		const order: string[] = [];
		const p = revealWindow({
			settle: async () => {},
			fonts: () => new Promise<void>((r) => (release = r)),
			paint: async () => void order.push('paint'),
			show: async () => void order.push('show')
		});
		await Promise.resolve();
		await Promise.resolve();
		expect(order).toEqual([]);
		release();
		await p;
		expect(order).toEqual(['paint', 'show']);
	});

	test('a fonts step that fails still shows the window', async () => {
		let shown = false;
		const ok = await revealWindow({
			settle: async () => {},
			fonts: async () => {
				throw new Error('boom');
			},
			paint: async () => {},
			show: async () => void (shown = true)
		});
		expect(ok).toBe(true);
		expect(shown).toBe(true);
	});
});
