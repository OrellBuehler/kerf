import { describe, expect, test } from 'bun:test';
import {
	clampEdge,
	frameIndex,
	frameTime,
	nearestWithin,
	nextDown,
	onFrame,
	quantizeFade,
	quantizeSpanStart,
	quantizeTime,
	snapToFrame,
	splitPoint,
	startBefore,
	trimEdit,
	WELD_EPS
} from './frames';
import { clipDuration, type Clip } from './types';

const RATES = [24, 25, 30, 50, 60, 24000 / 1001, 30000 / 1001, 60000 / 1001];

/** A deterministic stream of "pointer positions" — no flakiness. */
function rng(seed: number) {
	let s = seed >>> 0;
	return () => {
		s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
		return s / 4294967296;
	};
}

describe('snapToFrame', () => {
	test('lands on a boundary of the rate, for every common rate', () => {
		const rand = rng(1);
		for (const fps of RATES) {
			for (let i = 0; i < 400; i++) {
				const t = rand() * 600;
				const s = snapToFrame(t, fps);
				expect(onFrame(s, fps)).toBe(true);
				expect(Math.abs(s - t)).toBeLessThanOrEqual(0.5 / fps + 1e-12);
			}
		}
	});

	test('a frame is one division of an integer, so equal frames are equal doubles', () => {
		for (const fps of RATES) {
			for (const k of [0, 1, 7, 29, 30, 1799, 107_999]) {
				// however the position was reached, the same frame comes out bit for bit
				expect(snapToFrame(k / fps + 3e-7, fps)).toBe(frameTime(k, fps));
				expect(snapToFrame(k / fps - 3e-7, fps)).toBe(frameTime(k, fps));
				expect(snapToFrame(frameTime(k, fps), fps)).toBe(frameTime(k, fps));
			}
		}
	});

	test('rounds to the nearer frame', () => {
		expect(snapToFrame(1.0, 30)).toBe(30 / 30);
		expect(snapToFrame(1 / 30 + 0.4 / 30, 30)).toBe(1 / 30);
		expect(snapToFrame(1 / 30 + 0.6 / 30, 30)).toBe(2 / 30);
		expect(frameIndex(0.5, 24)).toBe(12);
	});

	test('never produces -0, and leaves an unusable rate or time alone', () => {
		expect(Object.is(snapToFrame(-0.001, 30), 0)).toBe(true);
		for (const fps of [0, -1, NaN, Infinity]) expect(snapToFrame(1.234, fps)).toBe(1.234);
		expect(snapToFrame(NaN, 30)).toBeNaN();
	});

	test('onFrame is false off the grid and for a bad rate', () => {
		expect(onFrame(1 / 30, 30)).toBe(true);
		expect(onFrame(1 / 30 + 0.001, 30)).toBe(false);
		expect(onFrame(1, 0)).toBe(false);
	});
});

describe('quantizeTime', () => {
	const fps = 30;

	test('a magnet within reach wins, exactly and unrounded', () => {
		const beat = 1.2345; // off the frame grid on purpose
		expect(quantizeTime(1.24, { fps, magnets: [beat], threshold: 0.05 })).toBe(beat);
	});

	test('with no magnet in reach the frame applies', () => {
		const t = quantizeTime(1.21, { fps, magnets: [5, 9], threshold: 0.05 });
		expect(t).toBe(frameTime(36, fps));
	});

	test('snapping off still quantizes: frames are not a magnet', () => {
		expect(quantizeTime(1.21, { fps, magnets: [], threshold: 0 })).toBe(frameTime(36, fps));
		expect(quantizeTime(1.21, { fps })).toBe(frameTime(36, fps));
	});

	test('picks the nearest of several magnets', () => {
		expect(quantizeTime(2.0, { fps, magnets: [1.97, 2.02, 2.04], threshold: 0.1 })).toBe(2.02);
	});

	test('welds to an edge that is a hair off the frame it landed on', () => {
		// An edge computed as start + length/speed, one ULP past its frame.
		const edge = frameTime(61, fps) + 4.4e-16;
		expect(edge).not.toBe(frameTime(61, fps));
		expect(quantizeTime(edge + 0.001, { fps, welds: [edge] })).toBe(edge);
		// ...but an edge a real distance away is not welded to
		expect(quantizeTime(frameTime(61, fps), { fps, welds: [frameTime(61, fps) + 1e-3] })).toBe(frameTime(61, fps));
		expect(WELD_EPS).toBeLessThan(1e-3);
	});
});

describe('quantizeSpanStart (move)', () => {
	const fps = 30;
	const dur = 2.4333333333333336;

	test('the start lands on a frame', () => {
		const s = quantizeSpanStart(4.0123, dur, { fps });
		expect(s).toBe(frameTime(120, fps));
	});

	test('a magnet for the head or for the tail (as a start target) wins', () => {
		const magnets = [10, 14.5 - dur];
		expect(quantizeSpanStart(10.03, dur, { fps, magnets, threshold: 0.1 })).toBe(10);
		expect(quantizeSpanStart(14.5 - dur + 0.02, dur, { fps, magnets, threshold: 0.1 })).toBe(14.5 - dur);
	});

	test('never moves before the start of the timeline', () => {
		expect(quantizeSpanStart(-3, dur, { fps })).toBe(0);
		expect(quantizeSpanStart(-3, dur, { fps, magnets: [-1], threshold: 5 })).toBe(0);
	});

	test('butts a neighbour exactly even with snapping off, at either end', () => {
		const next = frameTime(90, fps) + 4.4e-16; // the neighbour's start, a hair past frame 90
		const head = quantizeSpanStart(frameTime(30, fps), frameTime(60, fps), { fps, welds: [next] });
		// head at frame 30, 60 frames long: its tail is the neighbour's start, and never past it
		expect(head + frameTime(60, fps)).toBeLessThanOrEqual(next);
		expect(next - (head + frameTime(60, fps))).toBeLessThan(1e-12);
		const prevEnd = frameTime(30, fps) + 4.4e-16;
		expect(quantizeSpanStart(frameTime(30, fps), 1, { fps, welds: [prevEnd] })).toBe(prevEnd);
	});

	test('round once: a long run of moves ends on a frame boundary with no drift', () => {
		for (const fps of RATES) {
			const rand = rng(7);
			const origin = frameTime(90, fps);
			let start = origin;
			let raw = origin;
			for (let i = 0; i < 500; i++) {
				// Each gesture starts from where the last one landed, and the pointer
				// travels a sub-frame-noisy amount: the rounding must not remember it.
				const travelled = (rand() - 0.5) * 3.7;
				raw = start + travelled;
				start = quantizeSpanStart(raw, 1.5, { fps });
				expect(onFrame(start, fps)).toBe(true);
			}
			// bit-for-bit what a single gesture to the same raw position gives
			expect(start).toBe(quantizeSpanStart(raw, 1.5, { fps }));
			expect(start).toBe(frameTime(frameIndex(Math.max(0, raw), fps), fps));
			expect(start * fps).toBeCloseTo(Math.round(start * fps), 9);
		}
	});

	test('the result is a function of the raw position alone, not the path taken', () => {
		const fps = 30000 / 1001;
		const target = 12.3456;
		const direct = quantizeSpanStart(target, 2, { fps });
		let wandered = 0;
		for (const stop of [3.3, 9.9, 0.1, 40.4, 12.2, target]) wandered = quantizeSpanStart(stop, 2, { fps });
		expect(wandered).toBe(direct);
	});
});

describe('startBefore (a tail that butts a neighbour without passing it)', () => {
	/** The backend's own end of a clip: `timeline_start + duration`. */
	const endOf = (start: number, dur: number) => start + dur;

	test('nextDown is exactly one ULP', () => {
		for (const x of [1e-9, 0.1, 1, 2.4333333333333336, 600, 3600.5]) {
			const d = nextDown(x);
			expect(d).toBeLessThan(x);
			expect((d + x) / 2 === d || (d + x) / 2 === x).toBe(true); // nothing between them
		}
	});

	test('the naive tail - dur is sometimes past the tail; startBefore never is', () => {
		const rand = rng(31337);
		let naiveOver = 0;
		for (let i = 0; i < 20_000; i++) {
			const fps = RATES[Math.floor(rand() * RATES.length)];
			const tail = frameTime(1 + Math.floor(rand() * 60_000), fps) + (rand() < 0.5 ? 0 : (rand() - 0.5) * 1e-9);
			const dur = (1 + Math.floor(rand() * 6000)) / fps / (rand() < 0.5 ? 1 : 1.5);
			if (tail - dur <= 0) continue;
			if (endOf(tail - dur, dur) > tail) naiveOver++;
			const s = startBefore(tail, dur);
			expect(endOf(s, dur)).toBeLessThanOrEqual(tail);
			// ...and is the *latest* such start: one ULP up would pass the tail again, unless it is already exact
			if (s !== tail - dur) expect(endOf(Math.min(s * (1 + 2.3e-16), tail - dur), dur)).toBeLessThanOrEqual(tail + 1e-12);
			expect(Math.abs(endOf(s, dur) - tail)).toBeLessThan(1e-12);
		}
		// the premise: without the step-down this really did overlap
		expect(naiveOver).toBeGreaterThan(100);
	});

	test('a tail weld in quantizeSpanStart never passes the neighbour, the fuzz case that overlapped', () => {
		const rand = rng(8);
		for (let i = 0; i < 5000; i++) {
			const fps = RATES[Math.floor(rand() * RATES.length)];
			const dur = (30 + Math.floor(rand() * 900)) / fps / (rand() < 0.5 ? 1 : 1.25);
			const next = frameTime(200 + Math.floor(rand() * 5000), fps) + 4.4e-16 * Math.floor(rand() * 3);
			// the pointer puts the head on the frame that makes its tail land on `next`
			const start = quantizeSpanStart(next - dur + (rand() - 0.5) / fps / 3, dur, { fps, welds: [next] });
			if (Math.abs(start + dur - next) < 1e-6) expect(start + dur).toBeLessThanOrEqual(next);
		}
	});

	test('a start that is already exact is kept', () => {
		expect(startBefore(10, 2.5)).toBe(7.5);
		expect(startBefore(3, 3)).toBe(0);
	});
});

describe('clampEdge', () => {
	test('a real bound wins over the grid, even between frames', () => {
		const wall = 1.2345;
		expect(clampEdge(frameTime(40, 30), 0, wall)).toBe(wall);
		expect(clampEdge(0.5, 1, 2)).toBe(1);
		expect(clampEdge(1.5, 1, 2)).toBe(1.5);
	});
});

describe('splitPoint', () => {
	const fps = 30;

	test('a point comfortably inside is kept', () => {
		expect(splitPoint(5, 2, 9, fps)).toBe(5);
		const beat = 5.0123; // a magnet's off-grid time survives
		expect(splitPoint(beat, 2, 9, fps)).toBe(beat);
	});

	test('a point at or past an edge moves to the nearest interior frame', () => {
		expect(splitPoint(2, 2, 9, fps)).toBe(frameTime(61, fps));
		expect(splitPoint(1, 2, 9, fps)).toBe(frameTime(61, fps));
		expect(splitPoint(9, 2, 9, fps)).toBe(frameTime(269, fps));
		expect(splitPoint(99, 2, 9, fps)).toBe(frameTime(269, fps));
	});

	test('leaves at least half a frame on both sides', () => {
		const p = splitPoint(2.01, 2, 9, fps)!;
		expect(p - 2).toBeGreaterThanOrEqual(0.5 / fps - 1e-9);
		expect(onFrame(p, fps)).toBe(true);
		expect(9 - splitPoint(8.99, 2, 9, fps)!).toBeGreaterThanOrEqual(0.5 / fps - 1e-9);
	});

	test('an off-grid clip edge never produces a sliver', () => {
		// starts 0.1 ms before frame 60: the first interior frame is 61, not 60
		const start = frameTime(60, fps) - 1e-4;
		expect(splitPoint(start, start, 9, fps)).toBe(frameTime(61, fps));
	});

	test('a clip with no interior frame cannot be cut', () => {
		expect(splitPoint(1.01, 1, 1 + 0.9 / fps, fps)).toBeNull();
		expect(splitPoint(1.5, 1.5, 1.5, fps)).toBeNull();
		expect(splitPoint(5, 2, 9, 0)).toBe(5); // no rate: whatever is comfortably inside
		expect(splitPoint(2, 2, 9, 0)).toBeNull();
	});
});

describe('quantizeFade', () => {
	test('lands on a frame and never negative', () => {
		expect(quantizeFade(0.51, 5, 30)).toBe(frameTime(15, 30));
		expect(quantizeFade(-2, 5, 30)).toBe(0);
		expect(quantizeFade(0.01, 5, 30)).toBe(0); // rounds to no fade at all
	});

	test('never past the room left, and still on a frame when the room is not', () => {
		expect(quantizeFade(9, 2, 30)).toBe(2);
		const room = 2.01; // 60.3 frames
		const v = quantizeFade(9, room, 30);
		expect(v).toBe(frameTime(60, 30));
		expect(v).toBeLessThanOrEqual(room);
		expect(quantizeFade(1, -3, 30)).toBe(0);
	});
});

// ---- trimEdit: every field from one rounded position -------------------------

function clip(over: Partial<Clip> = {}): Clip {
	return {
		id: 'c',
		asset_id: 'a',
		source_in: 10,
		source_out: 22.5,
		timeline_start: 4,
		volume: 1,
		fade_in: 0,
		fade_out: 0,
		...over
	};
}

const applyEdit = (c: Clip, e: ReturnType<typeof trimEdit>): Clip => ({ ...c, ...e });
const endOf = (c: Clip) => c.timeline_start + clipDuration(c);

describe('trimEdit', () => {
	test('a forward right trim moves source_out only', () => {
		const e = trimEdit(clip(), 'r', 8);
		expect(Object.keys(e)).toEqual(['source_out']);
		expect(e.source_out).toBeCloseTo(10 + 4, 12);
	});

	test('a forward left trim moves source_in and the start so the right edge stays put', () => {
		const c = clip();
		const e = trimEdit(c, 'l', 5.5);
		expect(e.timeline_start).toBe(5.5);
		expect(e.source_in).toBeCloseTo(11.5, 12);
		expect(endOf(applyEdit(c, e))).toBeCloseTo(endOf(c), 12);
	});

	test('a reversed clip trims the other end of its source', () => {
		const c = clip({ speed: -1 });
		const r = trimEdit(c, 'r', 8);
		expect(Object.keys(r)).toEqual(['source_in']);
		expect(r.source_in).toBeCloseTo(22.5 - 4, 12);
		const l = trimEdit(c, 'l', 5);
		expect(l.source_out).toBeCloseTo(22.5 - 1, 12);
		expect(l.timeline_start).toBe(5);
		expect(endOf(applyEdit(c, l))).toBeCloseTo(endOf(c), 12);
	});

	test('speed scales the source distance, and the clip keeps its right edge', () => {
		for (const speed of [0.5, 1.5, 2, -0.25, -3]) {
			const c = clip({ speed });
			const e = trimEdit(c, 'l', 6.25);
			const after = applyEdit(c, e);
			expect(after.timeline_start).toBe(6.25);
			expect(endOf(after)).toBeCloseTo(endOf(c), 10);
			const r = applyEdit(c, trimEdit(c, 'r', 7.75));
			expect(endOf(r)).toBeCloseTo(7.75, 10);
		}
	});

	test('the edge a gesture edits lands on the frame it was rounded to, however many came before', () => {
		for (const fps of [24, 30, 30000 / 1001]) {
			for (const speed of [1, 1.5, -1, 0.5]) {
				const rand = rng(11 + speed * 100);
				let c = clip({ speed, source_in: 5, source_out: 505, timeline_start: 2 });
				const keepEnd = endOf(c);
				const keepStart = c.timeline_start;
				for (let i = 0; i < 200; i++) {
					const left = i % 2 === 0;
					// stay clear of the far edge so the clip never inverts
					const raw = left
						? c.timeline_start + (rand() - 0.5) * 0.9
						: endOf(c) + (rand() - 0.5) * 0.9;
					const pos = quantizeTime(raw, { fps });
					const before = c;
					c = applyEdit(c, trimEdit(c, left ? 'l' : 'r', pos));
					if (left) {
						// bit for bit the frame, not the frame plus whatever came before
						expect(c.timeline_start).toBe(pos);
						expect(onFrame(c.timeline_start, fps)).toBe(true);
						expect(endOf(c)).toBeCloseTo(endOf(before), 9);
					} else {
						expect(endOf(c)).toBeCloseTo(pos, 9);
						expect(c.timeline_start).toBe(before.timeline_start);
					}
				}
				// 200 trims on, the cut is where the last gesture put it
				expect(Math.abs(endOf(c) - keepEnd)).toBeLessThan(0.6 * 200);
				expect(c.source_out).toBeGreaterThan(c.source_in);
				expect(keepStart).toBe(2);
			}
		}
	});

	test('the opposite edge does not drift over a run of trims on one side', () => {
		const fps = 30;
		const rand = rng(99);
		let c = clip({ speed: 1.25, source_in: 0, source_out: 900, timeline_start: 0 });
		const end = endOf(c);
		for (let i = 0; i < 300; i++) {
			const pos = quantizeTime(c.timeline_start + (rand() - 0.5) * 0.4, { fps });
			c = applyEdit(c, trimEdit(c, 'l', pos));
		}
		expect(Math.abs(endOf(c) - end)).toBeLessThan(1e-9);
	});

	test('the same final position gives the same clip whatever the path to it', () => {
		const fps = 30;
		const base = clip({ speed: 1.5 });
		const target = quantizeTime(6.0123, { fps });
		const direct = applyEdit(base, trimEdit(base, 'l', target));
		let wandered = base;
		for (const raw of [4.5, 5.9, 4.2, 7.1, 6.0123]) {
			wandered = applyEdit(base, trimEdit(base, 'l', quantizeTime(raw, { fps })));
		}
		expect(wandered).toEqual(applyEdit(base, trimEdit(base, 'l', target)));
		expect(direct.timeline_start).toBe(frameTime(frameIndex(6.0123, fps), fps));
	});
});

describe('nearestWithin', () => {
	test('is strict about the threshold and returns the nearest', () => {
		expect(nearestWithin(1, [1.5], 0.5)).toBeNull();
		expect(nearestWithin(1, [1.4, 1.2, 0.95], 0.5)).toBe(0.95);
		expect(nearestWithin(1, [], 5)).toBeNull();
		expect(nearestWithin(1, [1], 0)).toBeNull();
	});
});
