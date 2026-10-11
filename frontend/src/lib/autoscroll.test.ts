import { describe, expect, test } from 'bun:test';
import {
	ARM_PX,
	createAutoscroll,
	edgeSpeed,
	EDGE_BAND_PX,
	followScroll,
	liveEnd,
	MAX_SPEED,
	type AutoscrollEnv,
	type ScrollRegion
} from './autoscroll';

describe('edgeSpeed', () => {
	const [lo, hi, max] = [100, 900, 1000];

	test('is nothing in the interior', () => {
		expect(edgeSpeed(500, lo, hi, max)).toBe(0);
		expect(edgeSpeed(lo + EDGE_BAND_PX, lo, hi, max)).toBe(0);
		expect(edgeSpeed(hi - EDGE_BAND_PX, lo, hi, max)).toBe(0);
	});

	test('is proportional to how far into the band the pointer is, signed toward the edge', () => {
		expect(edgeSpeed(hi - EDGE_BAND_PX / 2, lo, hi, max)).toBeCloseTo(500, 9);
		expect(edgeSpeed(hi, lo, hi, max)).toBeCloseTo(1000, 9);
		expect(edgeSpeed(lo + EDGE_BAND_PX / 4, lo, hi, max)).toBeCloseTo(-750, 9);
		expect(edgeSpeed(lo, lo, hi, max)).toBeCloseTo(-1000, 9);
	});

	test('is the most beyond the edge: the pointer left the window and the speed does not run on', () => {
		expect(edgeSpeed(hi + 500, lo, hi, max)).toBe(1000);
		expect(edgeSpeed(lo - 500, lo, hi, max)).toBe(-1000);
	});

	test('a window narrower than two bands has bands half its width', () => {
		expect(edgeSpeed(50, 0, 60, max)).toBeGreaterThan(0);
		expect(edgeSpeed(30, 0, 60, max)).toBe(0);
	});

	test('gives nothing for nonsense', () => {
		expect(edgeSpeed(Number.NaN, lo, hi, max)).toBe(0);
		expect(edgeSpeed(500, 900, 100, max)).toBe(0);
		expect(edgeSpeed(500, lo, hi, 0)).toBe(0);
	});
});

/** A scroller and a clock to drive the loop by hand. */
function harness(region: ScrollRegion, pointer: { x: number; y: number }, limit = { x: Infinity, y: Infinity }) {
	let t = 0;
	let next: (() => void) | null = null;
	const scrolls: { dx: number; dy: number }[] = [];
	let rereads = 0;
	const at = { x: 0, y: 0 };
	const env: AutoscrollEnv = {
		region: () => region,
		pointer: () => pointer,
		scrollBy(dx, dy) {
			scrolls.push({ dx, dy });
			const nx = Math.min(Math.max(at.x + dx, 0), limit.x);
			const ny = Math.min(Math.max(at.y + dy, 0), limit.y);
			const moved = { x: nx - at.x, y: ny - at.y };
			at.x = nx;
			at.y = ny;
			return moved;
		},
		scrolled: () => void rereads++,
		now: () => t,
		request: (cb) => {
			next = cb;
			return 1;
		},
		cancel: () => {
			next = null;
		}
	};
	return {
		env,
		scrolls,
		at,
		get rereads() {
			return rereads;
		},
		get pending() {
			return next !== null;
		},
		/** Advance the clock by `ms` and run the waiting frame. */
		frame(ms = 16) {
			t += ms;
			const cb = next;
			next = null;
			cb?.();
		}
	};
}

const REGION: ScrollRegion = { left: 200, right: 1000, top: 100, bottom: 500 };

describe('createAutoscroll', () => {
	test('scrolls toward an edge at a rate of px per second, and tells the gesture to read the pointer', () => {
		const h = harness(REGION, { x: 1000, y: 300 });
		const a = createAutoscroll(h.env);
		a.start({ x: 600, y: 300 });
		for (let i = 0; i < 10; i++) h.frame(20);
		const total = h.scrolls.reduce((n, s) => n + s.dx, 0);
		// 0.2 s at full speed (1400 px/s), whole pixels, remainders carried
		expect(total).toBeGreaterThanOrEqual(Math.floor(MAX_SPEED.x * 0.2) - 1);
		expect(total).toBeLessThanOrEqual(Math.ceil(MAX_SPEED.x * 0.2) + 1);
		expect(h.rereads).toBeGreaterThan(0);
		expect(h.scrolls.every((s) => s.dy === 0)).toBe(true);
	});

	test('the same real time is the same distance whatever the frame rate', () => {
		const run = (ms: number, n: number) => {
			const h = harness(REGION, { x: 1000, y: 300 });
			createAutoscroll(h.env).start({ x: 600, y: 300 });
			for (let i = 0; i < n; i++) h.frame(ms);
			return h.at.x;
		};
		expect(Math.abs(run(16, 25) - run(8, 50))).toBeLessThanOrEqual(2);
	});

	test('carries the fraction of a pixel a slow scroll would otherwise lose', () => {
		// 4 px into a 48 px band at 1400 px/s max ≈ 117 px/s: under 2 px per 16 ms frame.
		const h = harness(REGION, { x: 1000 - EDGE_BAND_PX + 4, y: 300 });
		createAutoscroll(h.env).start({ x: 600, y: 300 });
		for (let i = 0; i < 60; i++) h.frame(16);
		expect(h.at.x).toBeGreaterThan(80);
	});

	test('does nothing in the interior, and holds no frame callback once stopped', () => {
		const h = harness(REGION, { x: 600, y: 300 });
		const a = createAutoscroll(h.env);
		a.start({ x: 600, y: 300 });
		for (let i = 0; i < 5; i++) h.frame();
		expect(h.scrolls).toHaveLength(0);
		expect(a.running).toBe(true);
		a.stop();
		expect(a.running).toBe(false);
		expect(h.pending).toBe(false);
	});

	test('a stalled frame is not a leap', () => {
		const h = harness(REGION, { x: 1000, y: 300 });
		createAutoscroll(h.env).start({ x: 600, y: 300 });
		h.frame(5000);
		expect(h.at.x).toBeLessThanOrEqual(Math.ceil(MAX_SPEED.x * 0.05) + 1);
	});

	test('a press that began in the band waits until the pointer heads for the edge', () => {
		const pointer = { x: 990, y: 300 };
		const h = harness(REGION, pointer);
		createAutoscroll(h.env).start({ x: 990, y: 300 });
		pointer.x = 992; // jitter, not intent
		for (let i = 0; i < 5; i++) h.frame();
		expect(h.at.x).toBe(0);
		pointer.x = 990 + ARM_PX; // dragged toward the edge
		for (let i = 0; i < 5; i++) h.frame();
		expect(h.at.x).toBeGreaterThan(0);
	});

	test('a press in the band that is pulled away from the edge never scrolls, and a return does', () => {
		const pointer = { x: 990, y: 300 };
		const h = harness(REGION, pointer);
		createAutoscroll(h.env).start({ x: 990, y: 300 });
		pointer.x = 970; // still in the band, but toward the middle
		h.frame();
		expect(h.at.x).toBe(0);
		pointer.x = 600; // the interior: armed
		h.frame();
		pointer.x = 1000;
		for (let i = 0; i < 4; i++) h.frame();
		expect(h.at.x).toBeGreaterThan(0);
	});

	test('scrolls up and down too, unless the gesture can only go sideways', () => {
		const both = harness(REGION, { x: 600, y: 520 });
		createAutoscroll(both.env).start({ x: 600, y: 300 });
		for (let i = 0; i < 10; i++) both.frame(20);
		expect(both.at.y).toBeGreaterThan(0);

		const flat = harness(REGION, { x: 600, y: 520 });
		createAutoscroll(flat.env, { x: true, y: false }).start({ x: 600, y: 300 });
		for (let i = 0; i < 10; i++) flat.frame(20);
		expect(flat.at.y).toBe(0);
	});

	test('at the end of the scroll it stops asking the gesture to re-read', () => {
		const h = harness(REGION, { x: 1000, y: 300 }, { x: 30, y: 0 });
		createAutoscroll(h.env).start({ x: 600, y: 300 });
		for (let i = 0; i < 10; i++) h.frame(20);
		const rereads = h.rereads;
		for (let i = 0; i < 10; i++) h.frame(20);
		expect(h.at.x).toBe(30);
		expect(h.rereads).toBe(rereads);
	});

	test('stopping from inside the re-read is final', () => {
		const h = harness(REGION, { x: 1000, y: 300 });
		const a = createAutoscroll({ ...h.env, scrolled: () => a.stop() });
		a.start({ x: 600, y: 300 });
		h.frame(20);
		expect(a.running).toBe(false);
		expect(h.pending).toBe(false);
	});
});

describe('followScroll', () => {
	test('nothing while the playhead is inside the margins', () => {
		expect(followScroll({ laneX: 500, scrollLeft: 0, viewW: 1000, playing: true })).toBeNull();
		expect(followScroll({ laneX: 500, scrollLeft: 0, viewW: 1000, playing: false })).toBeNull();
	});

	test('while playing the view stays with the playhead, pinned just inside the edge it runs toward', () => {
		const r = followScroll({ laneX: 1000, scrollLeft: 0, viewW: 1000, playing: true })!;
		expect(r.smooth).toBe(false);
		expect(r.left).toBe(1000 - (1000 - 80));
		// the next frame of playback moves it on by the same amount the playhead moved
		const r2 = followScroll({ laneX: 1012, scrollLeft: r.left, viewW: 1000, playing: true })!;
		expect(r2.left - r.left).toBe(12);
	});

	test('playing backwards pins the left margin', () => {
		const r = followScroll({ laneX: 1000, scrollLeft: 1000, viewW: 1000, playing: true })!;
		expect(r.left).toBe(920);
	});

	test('a jump off screen puts the playhead a quarter in, smoothly, and never before 0', () => {
		expect(followScroll({ laneX: 5000, scrollLeft: 0, viewW: 1000, playing: false })).toEqual({ left: 4750, smooth: true });
		expect(followScroll({ laneX: 10, scrollLeft: 4000, viewW: 1000, playing: false })).toEqual({ left: 0, smooth: true });
	});

	test('a window with no width follows nothing', () => {
		expect(followScroll({ laneX: 5, scrollLeft: 0, viewW: 0, playing: true })).toBeNull();
	});
});

describe('liveEnd', () => {
	test('is the cut until a gesture reaches past it', () => {
		expect(liveEnd(20)).toBe(20);
		expect(liveEnd(20, null, undefined, 12)).toBe(20);
		expect(liveEnd(20, 31.5, 25)).toBe(31.5);
		expect(liveEnd(20, Number.NaN, -Infinity)).toBe(20);
		expect(liveEnd(-3)).toBe(0);
	});
});
